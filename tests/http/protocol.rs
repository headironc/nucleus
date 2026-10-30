use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*};

#[test]
fn http_extractors_preserve_authentication_and_precede_body_rejections() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop"),
                supplier.client(),
                message_at(sms.address(), sms.authority()),
            )
            .await
            .expect("startup");
            let (base, stop, task) = running(application).await;
            let actor = issued_session_for(&base, &mut sms, "13900000000").await;
            let client = http_client();
            // Stay below the real write budget while testing extraction precedence.
            let mut pace = tokio::time::interval(Duration::from_millis(110));
            pace.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            let source = std::cell::Cell::new(1_u16);
            let request = |method: reqwest::Method, path: &str| {
                let address = source.get();
                source.set(address + 1);
                client
                    .request(method, format!("{base}/api/v1{path}"))
                    .header("operation-id", Uuid::new_v4().to_string())
                    .header("session-id", &actor.0)
                    .header("cookie", &actor.1)
                    .header(
                        "x-real-ip",
                        format!("203.0.{}.{}", 180 + address / 250, address % 250 + 1),
                    )
            };
            for route in [
                "/workforce/invitations/candidates",
                "/workforce/acceptances",
                "/workforce/grants/candidates",
                "/workforce/terminations/candidates",
                "/workforce/suspensions/candidates",
                "/workforce/resumptions/candidates",
                "/workforce/grants/{id}/suspensions",
                "/workforce/suspensions/{id}/resumptions",
                "/workforce/assignments/candidates",
                "/workforce/assignments",
                "/workforce/numbers/candidates",
                "/workforce/numbers/corrections/candidates",
                "/workforce/persons/{id}/numbers",
            ] {
                let path = route.replace("{id}", OPERATION);
                for (query, content_type, body, status) in [
                    (
                        "?unexpected=fictional-private-query",
                        "text/plain",
                        "{}",
                        400,
                    ),
                    (
                        "?unexpected=fictional-private-query",
                        "application/json",
                        "{}",
                        400,
                    ),
                    ("", "text/plain", "{}", 415),
                    ("", "application/json", "{}", 422),
                ] {
                    pace.tick().await;
                    let response = request(reqwest::Method::POST, &format!("{path}{query}"))
                        .header("content-type", content_type)
                        .body(body)
                        .send()
                        .await
                        .expect("JSON extraction boundary");
                    assert_eq!(response.status(), status, "{route} {query} {content_type}");
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert!(!response.headers().contains_key("set-cookie"));
                    assert_eq!(
                        response.text().await.expect("safe envelope"),
                        r#"{"code":"invalid_input"}"#
                    );
                }
            }
            for (method, route) in [
                (reqwest::Method::GET, "/workforce/grants"),
                (reqwest::Method::GET, "/workforce/suspensions"),
                (reqwest::Method::GET, "/workforce/resumptions"),
                (reqwest::Method::GET, "/workforce/terminations"),
                (reqwest::Method::GET, "/workforce/current/grant"),
                (reqwest::Method::GET, "/workforce/grants/{id}"),
                (reqwest::Method::GET, "/workforce/current/role"),
                (reqwest::Method::GET, "/workforce/invitations/{id}"),
                (
                    reqwest::Method::POST,
                    "/workforce/invitations/{id}/withdrawals",
                ),
                (reqwest::Method::POST, "/workforce/grants/{id}/terminations"),
                (reqwest::Method::GET, "/affiliations"),
                (reqwest::Method::GET, "/affiliations/candidates/{id}"),
                (reqwest::Method::GET, "/persons/{id}/affiliation"),
                (reqwest::Method::GET, "/accounts/current/affiliation"),
                (reqwest::Method::GET, "/accounts/current/affiliations"),
                (reqwest::Method::GET, "/persons/{id}/affiliations"),
                (reqwest::Method::GET, "/workforce/current/numbers"),
                (reqwest::Method::GET, "/accounts/{id}/avatar"),
                (reqwest::Method::GET, "/mobile-changes/{id}"),
                (reqwest::Method::GET, "/revocations"),
            ] {
                let path = route.replace("{id}", OPERATION);
                let valid_query = if route == "/revocations" {
                    format!("?operation_id={OPERATION}")
                } else {
                    String::new()
                };
                for (query, length, status) in [
                    ("?unexpected=fictional-private-query", 8193, 400),
                    (valid_query.as_str(), 8193, 413),
                    (valid_query.as_str(), 1, 400),
                ] {
                    pace.tick().await;
                    let response = request(method.clone(), &format!("{path}{query}"))
                        .header("content-type", "application/json")
                        .body(vec![b'x'; length])
                        .send()
                        .await
                        .expect("byte body boundary");
                    assert_eq!(response.status(), status, "{route} {query} {length}");
                    assert_eq!(
                        response.text().await.expect("safe envelope"),
                        r#"{"code":"invalid_input"}"#
                    );
                }
            }
            for route in [
                "/assignments",
                "/assignments/roles",
                "/roles/changes",
                "/roles",
                "/workforce/roles/changes",
                "/workforce/roles",
                "/workforce/assignments",
                "/accounts/{id}/suspensions",
                "/accounts/current/corrections",
                "/persons/{id}/corrections",
            ] {
                let path = route.replace("{id}", OPERATION);
                for method in [reqwest::Method::GET, reqwest::Method::HEAD] {
                    pace.tick().await;
                    let response = request(method.clone(), &format!("{path}?page=0"))
                        .send()
                        .await
                        .expect("pagination boundary");
                    assert_eq!(response.status(), 400, "{route}");
                    assert_eq!(
                        response.text().await.expect("body"),
                        if method == reqwest::Method::HEAD {
                            ""
                        } else {
                            r#"{"code":"invalid_input"}"#
                        }
                    );
                }
            }
            for (method, path, authenticated, origin, status, code) in [
                (
                    reqwest::Method::GET,
                    "/roles?page=0",
                    false,
                    "https://example.com",
                    401,
                    "authentication_invalid",
                ),
                (
                    reqwest::Method::GET,
                    "/roles?page=0",
                    true,
                    "https://wrong.invalid",
                    403,
                    "permission_denied",
                ),
                (
                    reqwest::Method::PUT,
                    "/roles/invalid",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/workforce/grants/invalid",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/accounts/invalid/avatar",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/mobile-changes/invalid",
                    false,
                    "https://example.com",
                    401,
                    "authentication_invalid",
                ),
                (
                    reqwest::Method::GET,
                    "/mobile-changes/invalid",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/persons/invalid/affiliation",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/persons/invalid/corrections",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/persons/invalid/numbers",
                    false,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/grants/invalid/suspensions",
                    false,
                    "https://example.com",
                    401,
                    "authentication_invalid",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/grants/invalid/suspensions",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/suspensions/invalid/resumptions",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/grants/invalid/terminations",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::GET,
                    "/workforce/invitations/invalid",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
                (
                    reqwest::Method::POST,
                    "/workforce/invitations/invalid/withdrawals",
                    true,
                    "https://example.com",
                    400,
                    "invalid_input",
                ),
            ] {
                let mut probe = request(method, path)
                    .header("origin", origin)
                    .body("{}")
                    .build()
                    .expect("precedence request");
                if !authenticated {
                    probe.headers_mut().remove("cookie");
                    probe.headers_mut().remove("session-id");
                }
                pace.tick().await;
                let response = client
                    .execute(probe)
                    .await
                    .expect("authentication and path boundary");
                assert_eq!(response.status(), status, "{path}");
                assert_eq!(
                    response
                        .json::<serde_json::Value>()
                        .await
                        .expect("safe envelope"),
                    serde_json::json!({"code": code})
                );
            }
            stop.send(()).expect("stop");
            let (application, ended) = task.await.expect("process");
            ended.expect("drained");
            drop(application);
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 1);
            assert!(
                timeout(Duration::from_millis(30), sms.request())
                    .await
                    .is_err()
            );
            sms.stop().await;
        })
        .await
        .expect("bounded extraction acceptance");
    });
}

#[test]
fn http_validates_requests_before_business_work_and_describes_actual_routes() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        let supplier = Supplier::new().await;
        let name = database(&server).await;
        let proxy = fixture::database_proxy::Proxy::start(&server).await;
        let application = Application::start(
            proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"),
            process::Settings::new(Duration::from_secs(30)).expect("process settings"),
            supplier.client(), message().await,
        ).await.expect("start through proxy");
        let (base, stop, task) = running(application).await;
        let client = http_client();
        // Retired root entries are ordinary unknown paths: JSON 404 through the common middleware,
        // with no business work and no supplier call; nothing is redirected or aliased.
        let calls = supplier.calls.load(Ordering::SeqCst);
        let public = client.get(format!("{base}/api/v1/settings"))
            .header("operation-id", Uuid::new_v4().to_string())
            .header("cookie", "fictional-private-cookie")
            .send().await.expect("public settings");
        assert_eq!(public.status(), 200);
        assert_eq!(public.headers()["cache-control"], "no-store");
        assert!(!public.headers().contains_key("set-cookie"));
        assert!(!public.headers().contains_key("operation-id"));
        assert_eq!(public.json::<serde_json::Value>().await.expect("settings JSON"),
            serde_json::json!({"code":"ok","settings":{"challenge":{"turnstile":{"siteKey":"fixture-site-key"}}}}));
        for operation in ["invalid", ""] {
            let response = client.get(format!("{base}/api/v1/settings"))
                .header("operation-id", operation).send().await.expect("invalid header");
            assert_eq!(response.status(), 400);
        }
        let head = client.head(format!("{base}/api/v1/settings"))
            .header("operation-id", Uuid::new_v4().to_string()).send().await.expect("settings HEAD");
        assert_eq!(head.status(), 200);
        assert!(head.bytes().await.expect("empty HEAD").is_empty());
        for (method, path, status, code) in [
            (reqwest::Method::POST, "/codes", 404, "resource_not_found"),
            (reqwest::Method::GET, "/accounts/current", 404, "resource_not_found"),
            (reqwest::Method::GET, "/openapi.json", 404, "resource_not_found"),
            (reqwest::Method::GET, "/api/v1/openapi/v1.json", 404, "resource_not_found"),
            (reqwest::Method::GET, "/api/v1/openapi.json", 404, "resource_not_found"),
            (reqwest::Method::GET, "/api/v1/health/live", 404, "resource_not_found"),
            (reqwest::Method::GET, "/api/v1/unknown", 404, "resource_not_found"),
            (reqwest::Method::GET, "/unknown", 404, "resource_not_found"),
            (reqwest::Method::DELETE, "/api/v1/codes", 405, "method_not_allowed"),
            (reqwest::Method::DELETE, "/health/live", 405, "method_not_allowed"),
        ] {
            let response = client.request(method.clone(), format!("{base}{path}")).header("operation-id", OPERATION).header("origin", "https://example.com").header("x-real-ip", "198.51.100.61")
                .json(&serde_json::json!({"mobile":"13800000000","token":"fictional-token"})).send().await.expect("routing probe");
            assert_eq!(response.status(), status, "{method} {path}");
            assert_eq!(response.headers()["cache-control"], "no-store", "{path}");
            assert_eq!(response.json::<serde_json::Value>().await.expect("JSON fallback"), serde_json::json!({"code":code}), "{method} {path}");
        }
        for path in ["/codes", "/api/v1/unknown", "/unknown"] {
            let response = client.get(format!("{base}{path}")).send().await.expect("unidentified probe");
            assert_eq!(response.status(), 400, "fallbacks keep the common identifier check: {path}");
        }
        assert_eq!(supplier.calls.load(Ordering::SeqCst), calls, "retired paths never reach business work");
        for path in ["/health/live", "/health/ready", "/openapi/v1.json"] {
            let operation_id = if path == "/openapi/v1.json" { "invalid".to_owned() } else { OPERATION.to_uppercase() };
            let response = client.get(format!("{base}{path}")).header("operation-id", &operation_id).send().await.expect("technical endpoint");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let bytes = response.bytes().await.expect("JSON bytes");
            let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
            if path == "/openapi/v1.json" {
                if let Some(reference) = std::env::var_os("NUCLEUS_OPENAPI_REFERENCE") {
                    assert_eq!(bytes.as_ref(), std::fs::read(reference).expect("OpenAPI reference"));
                }
                if let Some(destination) = std::env::var_os("NUCLEUS_OPENAPI_EXPORT") {
                    std::fs::write(destination, &bytes).expect("export actual loopback OpenAPI");
                }
                assert!(value.get("operationId").is_none());
                assert_eq!(value["openapi"], "3.1.0");
                let credential = &value["components"]["schemas"]["session.Credential"];
                let message = credential["oneOf"].as_array().expect("credential variants")
                    .iter().find(|variant| variant["properties"]["code"].is_object())
                    .expect("SMS credential");
                let mut required = message["required"].as_array().expect("required SMS fields")
                    .iter().map(|field| field.as_str().expect("field name")).collect::<Vec<_>>();
                required.sort_unstable();
                assert_eq!(required, ["code", "id", "mobile", "type"]);
                assert_eq!(message["maxProperties"], required.len());
                assert_eq!(message["properties"]["mobile"]["pattern"], value["components"]["schemas"]["Input"]["properties"]["mobile"]["pattern"]);
                assert_eq!(message["properties"]["mobile"]["minLength"], 11);
                assert_eq!(message["properties"]["mobile"]["maxLength"], 11);
                let secret = &value["components"]["schemas"]["session.Input"]["properties"]["secret"];
                assert_eq!(secret["minLength"], 43);
                assert_eq!(secret["maxLength"], 43);
                assert_eq!(secret["pattern"], value["components"]["schemas"]["session.Registration"]["properties"]["token"]["pattern"]);
                for (name, fields) in [
                    ("session.Session", vec!["expiresAt", "id"]),
                    ("session.Registration", vec!["expiresAt", "id", "token"]),
                ] {
                    let schema = &value["components"]["schemas"][name];
                    let mut required = schema["required"].as_array().expect("required projection fields")
                        .iter().map(|field| field.as_str().expect("field name")).collect::<Vec<_>>();
                    required.sort_unstable();
                    assert_eq!(required, fields);
                    assert_eq!(schema["maxProperties"], required.len());
                }
                let receipts = value["components"]["schemas"]["session.Receipt"]["oneOf"].as_array().expect("R1 receipt variants");
                assert_eq!(receipts.len(), 2);
                for (receipt, field) in receipts.iter().zip(["session", "registration"]) {
                    assert_eq!(receipt["required"], serde_json::json!(["code", field]));
                    assert_eq!(receipt["maxProperties"], 2);
                }
                assert_eq!(message["properties"]["code"]["pattern"], "^[0-9]{6}$");
                for schema in ["transfer.Input", "transfer.CompletionInput"] {
                    assert_eq!(value["components"]["schemas"][schema]["properties"]["code"]["pattern"], "^[0-9]{6}$");
                }
                let paths = value["paths"].as_object().expect("paths");
                assert_eq!(paths.len(), 87);
                assert_eq!(paths["/api/v1/sessions"]["get"]["operationId"], "listOwnSessions");
                assert_eq!(paths["/api/v1/sessions/{id}"]["delete"]["operationId"], "revokeOtherSession");
                let active = &value["components"]["schemas"]["session.Active"];
                assert_eq!(active["required"], serde_json::json!(["id", "source", "createdAt", "current"]));
                assert_eq!(active["maxProperties"], 4);
                assert_eq!(value["components"]["schemas"]["session.Source"]["enum"], serde_json::json!(["website", "mini"]));
                assert_eq!(paths["/api/v1/accounts/current/avatar"]["put"]["operationId"], "replaceAvatar");
                assert_eq!(paths["/api/v1/replacements/{id}"]["get"]["operationId"], "avatarReplacement");
                let upload_content = paths["/api/v1/accounts/current/avatar"]["put"]["requestBody"]["content"].as_object().expect("raw upload media");
                assert_eq!(upload_content.len(), 3);
                for media in ["image/png", "image/jpeg", "image/webp"] {
                    let schema = &upload_content[media]["schema"];
                    let schema = schema["$ref"].as_str().map_or(schema, |reference| value.pointer(reference.strip_prefix('#').expect("local image schema")).expect("image schema"));
                    assert_eq!(schema["type"], "string");
                    assert_eq!(schema["format"], "binary");
                }
                assert!(paths.contains_key("/api/v1/affiliations"));
                assert!(paths.contains_key("/api/v1/settings"));
                assert!(paths.contains_key("/api/v1/workforce/directories"));
                assert!(paths.contains_key("/api/v1/workforce/recoveries") && paths.contains_key("/api/v1/workforce/recoveries/candidates"));
                // Every local schema reference resolves; utoipa does not collect path parameter schemas.
                let mut pending = vec![&value];
                while let Some(node) = pending.pop() {
                    match node {
                        serde_json::Value::Object(map) => {
                            if let Some(reference) = map.get("$ref").and_then(serde_json::Value::as_str) {
                                assert!(value.pointer(reference.strip_prefix('#').expect("local reference")).is_some(), "unresolved {reference}");
                            }
                            pending.extend(map.values());
                        }
                        serde_json::Value::Array(items) => pending.extend(items),
                        _ => {}
                    }
                }
                assert!(paths.contains_key("/api/v1/workforce/rules"));
                assert!(paths.contains_key("/api/v1/workforce/rules/{subtype}"));
                assert!(paths.contains_key("/api/v1/workforce/directories/departments/{id}/positions"));
                assert!(paths.contains_key("/api/v1/workforce/directories/{collection}"));
                assert!(paths.contains_key("/api/v1/workforce/directories/{collection}/{id}"));
                assert_eq!(paths["/api/v1/workforce/invitations/{id}"]["put"]["operationId"], "adjustWorkforceInvitation");
                // Business routes live under the backend prefix; only health and this document stay at the root.
                for path in paths.keys() {
                    assert!(path.starts_with("/api/v1/") || ["/health/live", "/health/ready", "/openapi/v1.json"].contains(&path.as_str()), "{path}");
                    assert!(!path.starts_with("/api/v1/openapi") && !path.starts_with("/api/v1/health"), "{path}");
                }
                assert!(value.get("servers").is_none(), "paths carry the real prefix");
                assert!(paths.contains_key("/api/v1/workforce/invitations/candidates") && paths.contains_key("/api/v1/workforce/acceptances"));
                assert!(paths.contains_key("/api/v1/accounts/current/affiliation") && paths.contains_key("/api/v1/accounts/current/affiliations"));
                assert!(paths.contains_key("/api/v1/persons/{id}/affiliations"));
                assert!(paths.contains_key("/api/v1/workforce/roles/changes"));
                assert!(paths.contains_key("/api/v1/workforce/numbers") && paths.contains_key("/api/v1/workforce/current/numbers"));
                assert!(paths.contains_key("/api/v1/workforce/persons/{id}/numbers") && paths.contains_key("/api/v1/workforce/persons/{id}/supplements"));
                assert!(paths.contains_key("/api/v1/workforce/persons/{id}/corrections"));
                assert_eq!(paths["/api/v1/workforce/persons/{id}/revisions"]["get"]["operationId"], "listWorkforceNumberRevisions");
                assert_eq!(paths["/api/v1/workforce/numbers/corrections/candidates"]["post"]["operationId"], "workforceNumberCorrectionCandidate");
                assert!(paths.contains_key("/api/v1/workforce/numbers/corrections/holders"));
                for route in ["/api/v1/workforce/grants/candidates", "/api/v1/workforce/grants", "/api/v1/workforce/terminations/candidates", "/api/v1/workforce/grants/{id}/terminations", "/api/v1/workforce/suspensions/candidates", "/api/v1/workforce/grants/{id}/suspensions", "/api/v1/workforce/resumptions/candidates", "/api/v1/workforce/suspensions/{id}/resumptions", "/api/v1/workforce/current/grant", "/api/v1/workforce/current/role", "/api/v1/workforce/invitations", "/api/v1/workforce/invitations/{id}", "/api/v1/workforce/invitations/{id}/withdrawals"] { assert!(paths.contains_key(route)); }
                for route in ["/api/v1/sessions", "/api/v1/registrations", "/api/v1/codes", "/health/live", "/health/ready", "/openapi/v1.json", "/api/v1/accounts/current", "/api/v1/accounts/current/role", "/api/v1/roles", "/api/v1/roles/changes", "/api/v1/roles/{id}", "/api/v1/avatars/{image}", "/api/v1/assignments", "/api/v1/assignments/candidates", "/api/v1/assignments/roles", "/api/v1/corrections", "/api/v1/corrections/candidates", "/api/v1/corrections/{person_id}"] { assert!(paths.contains_key(route)); }
                assert!(paths.contains_key("/api/v1/sessions/current") && paths.contains_key("/api/v1/revocations"));
                assert!(value["paths"]["/api/v1/codes"]["post"]["responses"]["429"]["headers"]["Retry-After"].is_object());
                for status in ["400", "413", "415", "422", "500"] {
                    assert!(value["paths"]["/api/v1/codes"]["post"]["responses"][status].is_object());
                }
                assert!(value.get("servers").is_none(), "paths carry the real prefix");
                assert!(paths.contains_key("/api/v1/accounts/current/affiliation") && paths.contains_key("/api/v1/accounts/current/affiliations"));
                assert!(paths.contains_key("/api/v1/persons/{id}/affiliations"));
                assert!(paths.contains_key("/api/v1/workforce/roles/changes"));
                assert!(paths.contains_key("/api/v1/workforce/numbers") && paths.contains_key("/api/v1/workforce/current/numbers"));
                assert!(paths.contains_key("/api/v1/workforce/numbers/candidates"));
                for route in ["/api/v1/workforce/grants/candidates", "/api/v1/workforce/grants", "/api/v1/workforce/terminations/candidates", "/api/v1/workforce/grants/{id}/terminations", "/api/v1/workforce/suspensions/candidates", "/api/v1/workforce/grants/{id}/suspensions", "/api/v1/workforce/resumptions/candidates", "/api/v1/workforce/suspensions/{id}/resumptions", "/api/v1/workforce/current/grant", "/api/v1/workforce/current/role", "/api/v1/workforce/invitations", "/api/v1/workforce/invitations/{id}", "/api/v1/workforce/invitations/{id}/withdrawals"] { assert!(paths.contains_key(route)); }
                for route in ["/api/v1/sessions", "/api/v1/registrations", "/api/v1/codes", "/health/live", "/health/ready", "/openapi/v1.json", "/api/v1/accounts/current", "/api/v1/accounts/current/role", "/api/v1/roles", "/api/v1/roles/changes", "/api/v1/roles/{id}", "/api/v1/avatars/{image}", "/api/v1/assignments", "/api/v1/assignments/candidates", "/api/v1/assignments/roles", "/api/v1/corrections", "/api/v1/corrections/candidates", "/api/v1/corrections/{person_id}"] { assert!(paths.contains_key(route)); }
                assert!(paths.contains_key("/api/v1/sessions/current") && paths.contains_key("/api/v1/revocations"));
                assert!(value["paths"]["/api/v1/codes"]["post"]["responses"]["429"]["headers"]["Retry-After"].is_object());
                for status in ["400", "413", "415", "422", "500"] {
                    assert!(value["paths"]["/api/v1/codes"]["post"]["responses"][status].is_object());
                }
                assert!(value.get("servers").is_none(), "paths carry the real prefix");
                assert!(paths.contains_key("/api/v1/accounts/current/affiliation") && paths.contains_key("/api/v1/accounts/current/affiliations"));
                assert!(paths.contains_key("/api/v1/persons/{id}/affiliations"));
                assert!(paths.contains_key("/api/v1/workforce/roles/changes"));
                assert!(paths.contains_key("/api/v1/workforce/numbers") && paths.contains_key("/api/v1/workforce/current/numbers"));
                for route in ["/api/v1/workforce/grants/candidates", "/api/v1/workforce/grants", "/api/v1/workforce/terminations/candidates", "/api/v1/workforce/grants/{id}/terminations", "/api/v1/workforce/suspensions/candidates", "/api/v1/workforce/grants/{id}/suspensions", "/api/v1/workforce/resumptions/candidates", "/api/v1/workforce/suspensions/{id}/resumptions", "/api/v1/workforce/current/grant", "/api/v1/workforce/current/role", "/api/v1/workforce/invitations", "/api/v1/workforce/invitations/{id}", "/api/v1/workforce/invitations/{id}/withdrawals"] { assert!(paths.contains_key(route)); }
                for route in ["/api/v1/sessions", "/api/v1/registrations", "/api/v1/codes", "/health/live", "/health/ready", "/openapi/v1.json", "/api/v1/accounts/current", "/api/v1/accounts/current/role", "/api/v1/roles", "/api/v1/roles/changes", "/api/v1/roles/{id}", "/api/v1/avatars/{image}", "/api/v1/assignments", "/api/v1/assignments/candidates", "/api/v1/assignments/roles", "/api/v1/corrections", "/api/v1/corrections/candidates", "/api/v1/corrections/{person_id}"] { assert!(paths.contains_key(route)); }
                assert!(paths.contains_key("/api/v1/sessions/current") && paths.contains_key("/api/v1/revocations"));
                assert!(value["paths"]["/api/v1/codes"]["post"]["responses"]["429"]["headers"]["Retry-After"].is_object());
                for status in ["400", "413", "415", "422", "500"] {
                    assert!(value["paths"]["/api/v1/codes"]["post"]["responses"][status].is_object());
                }
                assert!(value.get("servers").is_none(), "paths carry the real prefix");
                assert!(paths.contains_key("/api/v1/accounts/current/affiliation") && paths.contains_key("/api/v1/accounts/current/affiliations"));
                assert!(paths.contains_key("/api/v1/persons/{id}/affiliations"));
                assert!(paths.contains_key("/api/v1/workforce/roles/changes"));
                assert!(paths.contains_key("/api/v1/workforce/numbers") && paths.contains_key("/api/v1/workforce/current/numbers"));
                for route in ["/api/v1/workforce/grants/candidates", "/api/v1/workforce/grants", "/api/v1/workforce/terminations/candidates", "/api/v1/workforce/grants/{id}/terminations", "/api/v1/workforce/suspensions/candidates", "/api/v1/workforce/grants/{id}/suspensions", "/api/v1/workforce/resumptions/candidates", "/api/v1/workforce/suspensions/{id}/resumptions", "/api/v1/workforce/current/grant", "/api/v1/workforce/current/role", "/api/v1/workforce/invitations", "/api/v1/workforce/invitations/{id}", "/api/v1/workforce/invitations/{id}/withdrawals"] { assert!(paths.contains_key(route)); }
                for route in ["/api/v1/sessions", "/api/v1/registrations", "/api/v1/codes", "/health/live", "/health/ready", "/openapi/v1.json", "/api/v1/accounts/current", "/api/v1/accounts/current/role", "/api/v1/roles", "/api/v1/roles/changes", "/api/v1/roles/{id}", "/api/v1/avatars/{image}", "/api/v1/assignments", "/api/v1/assignments/candidates", "/api/v1/assignments/roles", "/api/v1/corrections", "/api/v1/corrections/candidates", "/api/v1/corrections/{person_id}"] { assert!(paths.contains_key(route)); }
                assert!(paths.contains_key("/api/v1/sessions/current") && paths.contains_key("/api/v1/revocations"));
                assert!(value["paths"]["/api/v1/codes"]["post"]["responses"]["429"]["headers"]["Retry-After"].is_object());
                for status in ["400", "413", "415", "422", "500"] {
                    assert!(value["paths"]["/api/v1/codes"]["post"]["responses"][status].is_object());
                }
                let origin = value["paths"]["/api/v1/codes"]["post"]["parameters"].as_array().expect("C1 parameters").iter().find(|parameter| parameter["name"] == "Origin").expect("origin");
                assert_eq!(origin["required"], true);
                let operation = value["paths"]["/api/v1/codes"]["post"]["parameters"].as_array().expect("C1 parameters").iter().find(|parameter| parameter["name"] == "Operation-Id").expect("operation ID");
                assert_eq!(operation["in"], "header");
                assert_eq!(operation["required"], true);
                assert_eq!(operation["schema"]["type"], "string");
                assert_eq!(operation["schema"]["format"], "uuid");
                let session_parameters = value["paths"]["/api/v1/sessions"]["post"]["parameters"].as_array().expect("session parameters");
                assert_eq!(session_parameters.len(), 1);
                let parameter = session_parameters.iter().find(|parameter| parameter["name"] == "Operation-Id").expect("required server input");
                assert_eq!(parameter["in"], "header");
                assert_eq!(parameter["required"], true);
                let schemas = &value["components"]["schemas"];
                let resolve = |schema: &serde_json::Value| {
                    let reference = schema["$ref"].as_str().expect("component reference");
                    value.pointer(reference.strip_prefix('#').expect("local reference")).expect("referenced schema")
                };
                let current = resolve(&value["paths"]["/api/v1/accounts/current"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]);
                let logout=resolve(&value["paths"]["/api/v1/sessions/current"]["delete"]["responses"]["200"]["content"]["application/json"]["schema"]);
                let completion=resolve(&logout["properties"]["completion"]);
                let completion_id=resolve(&completion["properties"]["id"]);
                assert_eq!(completion_id["type"],"string");
                assert_eq!(completion_id["format"],"uuid");
                assert_eq!(completion["required"],serde_json::json!(["id","count","revokedAt"]));
                let collection=resolve(&value["paths"]["/api/v1/revocations"]["get"]["responses"]["200"]["content"]["application/json"]["schema"]);
                assert_eq!(collection["required"],serde_json::json!(["code","original","count","revocations"]));
                let original = &value["components"]["schemas"]["revocation.Original"];
                assert_eq!(original["required"], serde_json::json!(["scope", "initiatorId"]));
                assert_eq!(resolve(&original["properties"]["initiatorId"])["format"], "uuid");
                assert_eq!(resolve(&original["properties"]["scope"])["enum"], serde_json::json!(["current", "all", "other"]));
                assert!(value["paths"]["/api/v1/revocations"]["head"].is_object());
                for (path,method) in [("/api/v1/sessions/current","delete"),("/api/v1/sessions","delete"),("/api/v1/revocations","get"),("/api/v1/revocations","head"),("/api/v1/accounts/current","get"),("/api/v1/accounts/current","head"),("/api/v1/accounts/current/role","get"),("/api/v1/accounts/current/role","head"),("/api/v1/roles","get"),("/api/v1/roles","head")] {
                    let operation = &value["paths"][path][method];
                    assert!(operation["responses"]["429"]["headers"]["Retry-After"].is_object());
                    let parameters = operation["parameters"].as_array().expect("same-origin parameters");
                    let session = parameters.iter().find(|parameter| parameter["name"] == "Session-Id").expect("required selected session");
                    let description = operation["description"].as_str().expect("operation contract");
                    let role_contract = matches!(path, "/api/v1/accounts/current/role" | "/api/v1/roles");
                    assert_eq!(session["required"], role_contract);
                    assert_eq!(parameters.iter().any(|parameter| parameter["name"] == "Authorization"), !role_contract);
                    if role_contract {
                        assert!(description.contains("docs/access/roles.md#"));
                    } else {
                        assert!(description.contains("Bearer"));
                        assert!(description.contains("Origin"));
                    }
                    if method == "delete" {
                        assert!(!parameters.iter().any(|parameter| parameter["name"] == "Origin"), "conditional website Origin is described per source");
                    } else {
                        assert!(!parameters.iter().any(|parameter| parameter["name"] == "Origin"));
                    }
                    for response in value["paths"][path][method]["responses"].as_object().expect("documented results").values() {
                        assert_eq!(response["headers"]["Cache-Control"]["description"],"no-store");
                    }
                }
                let account = resolve(&current["properties"]["account"]);
                for field in ["id", "personId", "revisionId"] {
                    let id = resolve(&account["properties"][field]);
                    assert_eq!(id["type"], "string");
                    assert_eq!(id["format"], "uuid");
                }
                let avatar = resolve(&account["properties"]["avatar"]);
                assert_eq!(avatar["properties"]["href"]["format"], "uri");
                assert!(avatar["properties"]["href"]["pattern"].as_str().expect("avatar path pattern").contains("/api/v1/avatars/"));
                assert_eq!(avatar["properties"]["mediaType"]["pattern"], r#"^image/webp(?![\s\S])"#);
                for field in ["width", "height"] {
                    assert_eq!(avatar["properties"][field]["minimum"], 1);
                    assert_eq!(avatar["properties"][field]["maximum"], 1024);
                }
                for name in ["current.Receipt", "current.Account", "current.Binding", "person.number.Suffix", "avatar.Current"] {
                    let schema = &value["components"]["schemas"][name];
                    let required = schema["required"].as_array().expect("required private projection fields");
                    assert_eq!(schema["maxProperties"], required.len());
                    assert_eq!(schema["properties"].as_object().expect("private projection properties").len(), required.len());
                }
                let revision = resolve(&avatar["properties"]["revision"]);
                assert_eq!(revision["type"], "string");
                assert_eq!(revision["format"], "uuid");
                assert!(account["required"].as_array().expect("required account fields").contains(&serde_json::json!("binding")));
                let binding = account["properties"]["binding"]["oneOf"].as_array().expect("nullable binding alternatives");
                assert!(binding.iter().any(|schema| schema["type"] == "null"));
                let binding = resolve(binding.iter().find(|schema| schema["$ref"].is_string()).expect("bound identity schema"));
                let revision = resolve(&binding["properties"]["revision"]);
                assert_eq!(revision["type"], "string");
                assert_eq!(revision["format"], "uuid");
                let avatar = &value["paths"]["/api/v1/avatars/{image}"]["get"];
                for status in ["400","403","404","429","503"] { assert!(avatar["responses"][status].is_object()); }
                let image = resolve(&avatar["responses"]["200"]["content"]["image/webp"]["schema"]);
                assert_eq!(image["type"],"string"); assert_eq!(image["format"],"binary");
                assert!(avatar["responses"]["200"]["headers"]["Cache-Control"]["description"].as_str().expect("cache contract").contains("private"));
                assert_eq!(avatar["responses"]["403"]["headers"]["Cache-Control"]["description"],"no-store");
                let challenge_input = resolve(&value["paths"]["/api/v1/codes"]["post"]["requestBody"]["content"]["application/json"]["schema"]);
                assert!(challenge_input["properties"].get("mobile").is_some());
                assert!(challenge_input["properties"].get("token").is_some());
                assert!(challenge_input["properties"].get("credential").is_none());
                assert_eq!(challenge_input["required"], serde_json::json!(["mobile", "token"]));
                assert_eq!(challenge_input["properties"]["mobile"]["type"], "string");
                assert_eq!(challenge_input["properties"]["token"]["type"], "string");
                assert_eq!(challenge_input["properties"]["token"]["minLength"], 1);
                assert_eq!(challenge_input["properties"]["token"]["maxLength"], 2048);
                let session_input = resolve(&value["paths"]["/api/v1/sessions"]["post"]["requestBody"]["content"]["application/json"]["schema"]);
                assert!(session_input["properties"].get("secret").is_some());
                assert!(session_input["properties"].get("credential").is_some());
                assert!(session_input["properties"].get("mobile").is_none());
                let credential = resolve(&session_input["properties"]["credential"]);
                let message = &credential["oneOf"][0]["properties"];
                assert_eq!(message["id"]["$ref"], "#/components/schemas/message.Id");
                let registration_input = resolve(&value["paths"]["/api/v1/registrations"]["post"]["requestBody"]["content"]["application/json"]["schema"]);
                assert_eq!(registration_input["required"], serde_json::json!(["id", "token", "name", "idNumber"]));
                assert_eq!(registration_input["properties"].as_object().expect("R2 input fields").len(), 4);
                assert_eq!(registration_input["properties"]["id"]["$ref"], "#/components/schemas/registration.Id");
                let name = resolve(&registration_input["properties"]["name"]);
                assert_eq!(name["type"], "string");
                assert!(name.get("maxLength").is_none(), "name length applies after normalization");
                assert!(name.get("pattern").is_none(), "R2 accepts text before normalization");
                let name = &account["properties"]["name"];
                assert_eq!(name["type"], "string");
                assert_eq!(name["minLength"], 1);
                assert_eq!(name["maxLength"], 64);
                assert!(name["pattern"].as_str().expect("canonical output name").contains(r#"\p{Cs}"#));
                let id_number = resolve(&account["properties"]["idNumber"]);
                assert_eq!(id_number["required"], serde_json::json!(["suffix"]));
                assert_eq!(id_number["properties"].as_object().expect("masked identity number").len(), 1);
                assert_eq!(id_number["properties"]["suffix"]["type"], "string");
                assert_eq!(id_number["properties"]["suffix"]["minLength"], 4);
                assert_eq!(id_number["properties"]["suffix"]["maxLength"], 4);
                assert_eq!(id_number["properties"]["suffix"]["pattern"], "^[0-9]{3}[0-9X]$");
                for status in ["200", "201"] {
                    let receipt = resolve(&value["paths"]["/api/v1/registrations"]["post"]["responses"][status]["content"]["application/json"]["schema"]);
                    assert_eq!(receipt["properties"].as_object().expect("R2 receipt fields").len(), 2);
                    let session = resolve(&receipt["properties"]["session"]);
                    let sources = session["oneOf"].as_array().expect("R2 source-specific session delivery");
                    assert_eq!(sources.len(), 2);
                    let website = sources.iter().map(&resolve).find(|session| session["properties"].get("token").is_none()).expect("website cookie projection");
                    let mini = sources.iter().map(&resolve).find(|session| session["properties"].get("token").is_some()).expect("mini Bearer projection");
                    assert_eq!(website["properties"].as_object().expect("website fields").len(), 2);
                    assert_eq!(mini["properties"].as_object().expect("mini fields").len(), 3);
                    assert_eq!(mini["required"],serde_json::json!(["id","token","expiresAt"]));
                    assert_eq!(mini["properties"]["token"]["minLength"], 43);
                    assert_eq!(mini["properties"]["token"]["maxLength"], 43);
                    assert!(value["paths"]["/api/v1/registrations"]["post"]["responses"][status]["headers"]["Set-Cookie"].is_object());
                }
                let challenge_result = resolve(&value["paths"]["/api/v1/codes"]["post"]["responses"]["200"]["content"]["application/json"]["schema"]);
                assert!(challenge_result["properties"].get("message").is_some());
                let session_result = resolve(&value["paths"]["/api/v1/sessions"]["post"]["responses"]["200"]["content"]["application/json"]["schema"]);
                let branches = session_result["oneOf"].as_array().expect("exclusive session results");
                assert_eq!(branches.len(), 2);
                for branch in branches {
                    let properties = branch["properties"].as_object().expect("session branch");
                    assert_eq!(properties.len(), 2);
                    if let Some(session) = properties.get("session") {
                        let sources = resolve(session)["oneOf"].as_array().expect("R1 source-specific sessions");
                        assert_eq!(sources.len(), 2);
                        let mut bearer = 0;
                        for source in sources {
                            let fields = resolve(source)["properties"].as_object().expect("session fields");
                            assert!(fields.contains_key("id") && fields.contains_key("expiresAt"));
                            if fields.contains_key("token") { bearer += 1; assert_eq!(fields.len(), 3); }
                            else { assert_eq!(fields.len(), 2); }
                        }
                        assert_eq!(bearer, 1);
                    } else {
                        let fields = resolve(&properties["registration"])["properties"].as_object().expect("registration fields");
                        assert_eq!(fields.len(), 3);
                        assert!(fields.contains_key("id") && fields.contains_key("token") && fields.contains_key("expiresAt"));
                        assert_eq!(fields["token"]["type"], "string");
                        assert_eq!(fields["token"]["minLength"], 43);
                        assert_eq!(fields["token"]["maxLength"], 43);
                        assert_eq!(fields["token"]["pattern"], "^[A-Za-z0-9_-]{42}[AEIMQUYcgkosw048]$");
                    }
                }
                let failure = &schemas["Failure"];
                assert_eq!(failure["maxProperties"], 1);
                assert_eq!(failure["properties"].as_object().expect("error fields").len(), 1);
                assert_eq!(failure["required"], serde_json::json!(["code"]));
                assert_eq!(failure["properties"]["code"]["type"], "string");
                assert!(failure["properties"]["code"].get("enum").is_none());
                assert!(failure["properties"]["code"].get("$ref").is_none());
                for name in ["Failure", "Receipt", "Live", "Ready"] {
                    let fields = &schemas[name]["properties"];
                    assert!(fields.get("operationId").is_none());
                    assert!(fields.get("operation_id").is_none());
                }
                let fields = &schemas["Message"]["properties"];
                assert_eq!(fields["id"]["$ref"], "#/components/schemas/message.Id");
                assert_eq!(schemas["message.Id"]["type"], "string");
                assert_eq!(schemas["message.Id"]["format"], "uuid");
                assert_eq!(fields["expiresAt"]["format"], "date-time");
                assert!(fields.get("expires_at").is_none());
                assert!(value["paths"]["/health/live"]["head"].is_object());
                assert!(value["paths"]["/health/live"]["head"]["responses"]["200"].get("content").is_none());
                for path in ["/health/live", "/health/ready"] {
                    for method in ["get", "head"] {
                        let operation = &value["paths"][path][method];
                        let parameter = operation["parameters"].as_array().expect("health parameters").iter().find(|parameter| parameter["name"] == "Operation-Id").expect("required request identity");
                        assert_eq!(parameter["in"], "header");
                        assert_eq!(parameter["required"], true);
                        assert_eq!(parameter["schema"]["type"], "string");
                        assert_eq!(parameter["schema"]["format"], "uuid");
                        assert!(operation["responses"]["400"].is_object());
                        if method == "head" {
                            assert!(operation["responses"]["400"].get("content").is_none());
                        }
                    }
                }
            } else {
                let field = if path == "/health/live" { "live" } else { "ready" };
                assert_eq!(value, serde_json::json!({"code":"ok",field:true}));
            }
            let request = client.head(format!("{base}{path}"));
            let request = if path == "/openapi/v1.json" { request } else { request.header("operation-id", &operation_id) };
            let response = request.send().await.expect("HEAD");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(response.bytes().await.expect("empty body").is_empty());
        }
        for path in ["/health/live", "/health/ready"] {
            for method in [reqwest::Method::GET, reqwest::Method::HEAD, reqwest::Method::POST] {
                for headers in [vec![], vec![""], vec!["invalid"], vec![OPERATION, OPERATION], vec![OPERATION, OTHER]] {
                    let mut request = client.request(method.clone(), format!("{base}{path}"));
                    for header in headers { request = request.header("operation-id", header); }
                    let response = request.send().await.expect("invalid health identifier");
                    assert_eq!(response.status(), 400);
                    assert_eq!(response.headers()["cache-control"], "no-store");
                    assert_eq!(response.headers()["x-content-type-options"], "nosniff");
                    if method == reqwest::Method::HEAD {
                        assert!(response.bytes().await.expect("empty HEAD rejection").is_empty());
                    } else {
                        assert_eq!(response.json::<serde_json::Value>().await.expect("health rejection"), serde_json::json!({"code":"invalid_input"}));
                    }
                }
            }
            let response = client.post(format!("{base}{path}")).header("operation-id", OPERATION).send().await.expect("health method refusal");
            assert_eq!(response.status(), 405);
            assert!(response.headers()["allow"].to_str().expect("allowed methods").contains("GET"));
            assert_eq!(response.json::<serde_json::Value>().await.expect("health method JSON"), serde_json::json!({"code":"method_not_allowed"}));
        }
        for (path, expected, code) in [("/api/v1/codes", 405, "method_not_allowed"), ("/api/v1/sessions", 400, "invalid_input"), ("/api/v1/registrations", 405, "method_not_allowed")] {
            let response = client.get(format!("{base}{path}")).header("operation-id", OPERATION).header("x-real-ip", "invalid").header("origin", "https://attacker.invalid").send().await.expect("route refusal");
            assert_eq!(response.status(), expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let value: serde_json::Value = response.json().await.expect("safe JSON");
            assert_eq!(value["code"], code);
            assert_eq!(value, serde_json::json!({"code":code}));
        }
        for path in ["/api/v1/sessions", "/api/v1/registrations", "/api/v1/codes", "/openapi/v1.json"] {
            let response = client.post(format!("{base}{path}")).send().await.expect("fallback identifier");
            assert_eq!(response.status(), 400);
            assert_eq!(response.json::<serde_json::Value>().await.expect("fallback error"), serde_json::json!({"code":"invalid_input"}));
            let response = client.head(format!("{base}{path}")).header("operation-id", OPERATION).send().await.expect("fallback HEAD");
            assert_eq!(response.status(), match path { "/openapi/v1.json" => 200, "/api/v1/sessions" => 401, _ => 405 });
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            assert!(response.bytes().await.expect("HEAD body").is_empty());
        }
        let plain = reqwest::Client::builder().no_proxy().timeout(Duration::from_secs(5)).build().expect("client without Origin");
        assert_eq!(plain.get(format!("{base}/health/live")).header("operation-id", OPERATION).send().await.expect("health needs no Origin").status(), 200);
        for sources in [vec![], vec![""], vec!["invalid"], vec!["192.0.2.1", "192.0.2.1"], vec!["192.0.2.1, 192.0.2.2"], vec!["192.0.2.1:80"]] {
            let mut request = plain.post(format!("{base}/api/v1/codes"))
                .header("operation-id", OPERATION)
                .header("origin", "https://example.com")
                .header("x-forwarded-for", "192.0.2.1")
                .body("unread body");
            for source in sources { request = request.header("x-real-ip", source); }
            let response = request.send().await.expect("source refusal");
            assert_eq!(response.status(), 400);
            assert_eq!(response.json::<serde_json::Value>().await.expect("source error"), serde_json::json!({"code":"invalid_input"}));
        }
        for (index, origins) in [vec![], vec![""], vec!["null"], vec!["https://example.com", "https://example.com"], vec!["https://attacker.invalid"]].into_iter().enumerate() {
            let mut request = plain.post(format!("{base}/api/v1/codes"))
                .header("operation-id", OTHER)
                .header("authorization", "Bearer fictional-ignored-token")
                .header("x-real-ip", format!("198.51.100.{}", index + 10))
                .body("unread body");
            for origin in origins { request = request.header("origin", origin); }
            let response = request.send().await.expect("Origin rejection");
            assert_eq!(response.status(), 403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("Origin error"), serde_json::json!({"code":"permission_denied"}));
        }
        for (index, (types, expected)) in [(vec![], 415), (vec!["text/plain"], 415), (vec!["application/problem+json"], 422), (vec!["application/json", "application/json"], 422)].into_iter().enumerate() {
            let mut request = client.post(format!("{base}/api/v1/codes")).header("operation-id", OTHER).header("x-real-ip", format!("198.51.100.{}", index + 30)).body("{}");
            for content in types { request = request.header("content-type", content); }
            let response = request.send().await.expect("media and field rejection");
            assert_eq!(response.status(), expected);
            assert_eq!(response.json::<serde_json::Value>().await.expect("media error"), serde_json::json!({"code":"invalid_input"}));
        }
        for headers in [vec![], vec![""], vec!["invalid"], vec![OPERATION, OTHER], vec!["00000000-0000-0000-0000-000000000000, ffffffff-ffff-ffff-ffff-ffffffffffff"]] {
            let mut request = client.post(format!("{base}/api/v1/codes")).body("not JSON");
            for header in headers { request = request.header("operation-id", header); }
            let response = request.send().await.expect("invalid identifier");
            assert_eq!(response.status(), 400);
            let value: serde_json::Value = response.json().await.expect("error JSON");
            assert_eq!(value["code"], "invalid_input");
            assert_eq!(value, serde_json::json!({"code":"invalid_input"}));
        }
        for (index, (body, expected)) in [
            (r#"{}"#.to_string(), 422),
            (r#"{"mobile":"13800000000","token":"a","token":"b"}"#.into(), 422),
            (r#"{"mobile":"13800000000","token":"a","extra":true}"#.into(), 422),
            (r#"{"mobile":13800000000,"token":"a"}"#.into(), 422),
            (r#"{"mobile":"+8613800000000","token":"a"}"#.into(), 422),
            (r#"{"mobile":"invalid"}"#.into(), 422),
            (r#"{"mobile":"13800000000","token":null}"#.into(), 422),
            (r#"{"mobile":"13800000000","token":""}"#.into(), 422),
            (r#"["13800000000", ""]"#.into(), 422),
            (format!(r#"{{"mobile":"13800000000","token":"{}"}}"#, "a".repeat(2049)), 422),
            (format!(r#"{{"mobile":"13800000000","token":"{}"}}"#, "界".repeat(2049)), 422),
            ("{".into(), 400),
            (r#"{"mobile":"13800000000","token":"a"} {}"#.into(), 400),
            (r#"{"mobile":"invalid","token":"a"} {}"#.into(), 422),
            (" ".repeat(8193), 413),
        ].into_iter().enumerate() {
            let response = client.post(format!("{base}/api/v1/codes")).header("operation-id", OTHER.to_uppercase()).header("x-real-ip", format!("192.0.2.{}",index+1)).header("content-type", "application/json").body(body).send().await.expect("invalid body");
            assert_eq!(response.status(), expected);
            let value: serde_json::Value = response.json().await.expect("safe response");
            assert_eq!(value, serde_json::json!({"code":"invalid_input"}));
        }
        for (source, origin, content, expected) in [("invalid", "https://example.com", "application/json",400), ("192.0.2.100", "https://attacker.invalid", "application/json",403), ("192.0.2.101", "https://example.com", "text/plain",415)] {
            let response = client.post(format!("{base}/api/v1/codes")).header("operation-id", OTHER).header("x-real-ip",source).header("origin",origin).header("content-type",content).body("{}").send().await.expect("boundary refusal");
            assert_eq!(response.status(),expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            let code = if expected == 403 { "permission_denied" } else { "invalid_input" };
            assert_eq!(response.json::<serde_json::Value>().await.expect("boundary error"), serde_json::json!({"code":code}));
        }
        for expected in [422,422,429] {
            let response = client.post(format!("{base}/api/v1/codes")).header("operation-id",OTHER).header("x-real-ip","198.51.100.1").header("content-type","application/json").body("{}").send().await.expect("ingress quota");
            assert_eq!(response.status(),expected);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(response.headers()["x-content-type-options"], "nosniff");
            if expected == 429 { assert!(response.headers()["retry-after"].to_str().expect("wait").parse::<u64>().expect("seconds") >= 1); }
            let code = if expected == 429 { "rate_limited" } else { "invalid_input" };
            assert_eq!(response.json::<serde_json::Value>().await.expect("quota error"), serde_json::json!({"code":code}));
        }
        assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),0);
        for body in [serde_json::json!({"mobile":"13800000000","token":"fictional-token"}), serde_json::json!(["13800000000", "fictional-token"])] {
            let response = client.post(format!("{base}/api/v1/codes")).header("operation-id",OPERATION).header("x-real-ip","203.0.113.1").json(&body).send().await.expect("C1 rejection");
            assert_eq!(response.status(),403);
            assert_eq!(response.json::<serde_json::Value>().await.expect("rejection")["code"],"verification_invalid");
        }
        assert_eq!(supplier.calls.load(std::sync::atomic::Ordering::SeqCst),1);
        proxy.arm(fixture::database_proxy::Fault::RewoundCurrent);
        let readiness = tokio::spawn(client.get(format!("{base}/health/ready")).header("operation-id", OPERATION).send());
        timeout(Duration::from_secs(2), proxy.intercepted()).await.expect("readiness observes rewound time");
        proxy.release();
        let response = readiness.await.expect("readiness task").expect("readiness");
        assert_eq!(response.status(),503);
        assert_eq!(response.json::<serde_json::Value>().await.expect("not ready"),serde_json::json!({"code":"dependency_unavailable"}));
        let response = client.head(format!("{base}/health/ready")).header("operation-id", OPERATION).send().await.expect("HEAD readiness");
        assert_eq!(response.status(),503);
        assert!(response.bytes().await.expect("empty readiness body").is_empty());
        assert_eq!(client.get(format!("{base}/health/live")).header("operation-id", OPERATION).send().await.expect("liveness").status(),200);
        stop.send(()).expect("stop");
        let (application,result) = timeout(Duration::from_secs(5),task).await.expect("stop budget").expect("HTTP task");
        result.expect("drained");
        drop(application);
        proxy.stop().await;
    });
}
