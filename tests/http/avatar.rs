use std::{sync::atomic::Ordering, time::Duration};

use nucleus::process::{self, Application};
use tokio::{runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::{application::*, authentication::*, database::*, supplier::*, suspension::*};

#[test]
fn accepted_avatar_upload_timeout_preserves_unknown_outcome_and_original_work() {
    use fixture::database_proxy::{Fault, Proxy};
    use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};
    use serde_json::Value;
    use tokio::{
        io::{AsyncReadExt as _, AsyncWriteExt as _},
        net::TcpStream,
        time::Instant,
    };

    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(75), async {
            let name = database(&server).await;
            let proxy = Proxy::start(&server).await;
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                proxy_connections(&name, &proxy), keys(0), services(8).expect("settings"),
                process::Settings::new(Duration::from_secs(5)).expect("stop budget"),
                supplier.client(), message_at(sms.address(), sms.authority()),
            ).await.expect("startup");
            let (base, stop, task) = running(application).await;
            let actor = issued_session_from(&base, &mut sms, "13900000000", "203.0.158.1").await;
            let client = http_client();
            let current: Value = client.get(format!("{base}/api/v1/accounts/current"))
                .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0)
                .header("cookie", &actor.1).header("x-real-ip", "203.0.158.2")
                .send().await.expect("current").error_for_status().expect("current success")
                .json().await.expect("current body");
            let revision = current["account"]["avatar"]["revision"].as_str().expect("revision");
            let mut png = Vec::new();
            PngEncoder::new(&mut png).write_image(&[89; 16], 2, 2, ExtendedColorType::Rgba8).expect("PNG");
            let blocker = connections(&server, &name, "postgres").recover().await.expect("isolated lock owner");
            let observer = connections(&server, &name, "clerk").recover().await.expect("independent observer");
            let blocker_pid: i32 = blocker.client().query_one("SELECT pg_backend_pid()", &[]).await.expect("blocker identity").get(0);
            blocker.client().batch_execute("BEGIN; LOCK TABLE identity.uploads IN SHARE MODE").await.expect("hold result INSERT only");
            let operation = Uuid::new_v4();
            let address = base.strip_prefix("http://").expect("loopback HTTP address").to_owned();
            let headers = format!("PUT /api/v1/accounts/current/avatar HTTP/1.1\r\nHost: {address}\r\nOrigin: https://example.com\r\nOperation-Id: {operation}\r\nSession-Id: {}\r\nCookie: {}\r\nX-Real-IP: 203.0.158.3\r\nAvatar-Revision: {revision}\r\nContent-Type: image/png\r\nContent-Length: {}\r\nConnection: close\r\n\r\n", actor.0, actor.1, png.len());
            proxy.arm(Fault::PausedCommit);
            let started = Instant::now();
            let caller = tokio::spawn(async move {
                let mut socket = TcpStream::connect(address).await.expect("upload connection");
                socket.write_all(headers.as_bytes()).await.expect("upload headers");
                // Spend part of the real 30-second budget without exceeding the 15-second body limit.
                tokio::time::sleep(Duration::from_secs(13)).await;
                socket.write_all(&png).await.expect("complete PNG body");
                let mut response = Vec::new();
                socket.read_to_end(&mut response).await.expect("bounded HTTP response");
                response
            });
            timeout(Duration::from_secs(18), proxy.intercepted()).await.expect("publication COMMIT held");
            // Complete publication within its write budget; later result recovery crosses the HTTP deadline.
            tokio::time::sleep(Duration::from_secs(8)).await;
            proxy.release();
            timeout(Duration::from_secs(5), async {
                loop {
                    let waiting: bool = observer.client().query_one(
                        "SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND query LIKE 'INSERT INTO identity.uploads %' AND $1=ANY(pg_blocking_pids(pid)))", &[&blocker_pid]
                    ).await.expect("exact result transaction waiting").get(0);
                    if waiting { break; }
                    tokio::task::yield_now().await;
                }
            }).await.expect("publication confirmed and result transaction reached");
            proxy.arm(Fault::DelayedCommit);
            blocker.client().batch_execute("COMMIT").await.expect("allow original result COMMIT");
            let backend = timeout(Duration::from_secs(5), proxy.intercepted()).await.expect("original result COMMIT held");
            let response = timeout(Duration::from_secs(12), caller).await.expect("HTTP total wait is bounded").expect("HTTP waiter");
            assert!(started.elapsed() >= Duration::from_secs(29), "the HTTP deadline wins over original recovery");
            let boundary = response.windows(4).position(|bytes| bytes == b"\r\n\r\n").expect("HTTP response boundary");
            assert!(response.starts_with(b"HTTP/1.1 503 "));
            assert_eq!(serde_json::from_slice::<Value>(&response[boundary + 4..]).expect("failure body")["code"], "outcome_unknown");
            assert!(!observer.client().query_one("SELECT EXISTS (SELECT 1 FROM identity.uploads WHERE operation_id=$1)", &[&operation])
                .await.expect("result still uncommitted").get::<_, bool>(0));
            assert!(observer.client().query_one("SELECT EXISTS (SELECT 1 FROM pg_stat_activity WHERE datname=current_database() AND pid=$1)", &[&backend])
                .await.expect("original backend still retained").get::<_, bool>(0));
            proxy.release();
            timeout(Duration::from_secs(10), async {
                loop {
                    let response = client.get(format!("{base}/api/v1/replacements/{operation}"))
                        .header("operation-id", Uuid::new_v4().to_string()).header("session-id", &actor.0)
                        .header("cookie", &actor.1).header("x-real-ip", "203.0.158.4")
                        .send().await.expect("read exact original after release");
                    if response.status() == 200 {
                        let result: Value = response.json().await.expect("original receipt");
                        assert_eq!(result["completion"]["id"], operation.to_string());
                        assert_eq!(result["completion"]["changed"], true);
                        assert_ne!(result["avatar"]["revision"], revision);
                        break;
                    }
                    assert_eq!(response.status(), 503);
                    assert_eq!(response.json::<Value>().await.expect("pending original")["code"], "outcome_unknown");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
            }).await.expect("same accepted work completes after HTTP timeout");
            assert_eq!(observer.client().query_one("SELECT count(*) FROM identity.uploads WHERE operation_id=$1", &[&operation])
                .await.expect("one original result").get::<_, i64>(0), 1);
            stop.send(()).expect("stop");
            let (application, ended) = task.await.expect("server task");
            ended.expect("drained accepted work"); drop(application);
            blocker.terminate().await.expect("blocker ended"); observer.terminate().await.expect("observer ended");
            proxy.stop().await; sms.stop().await;
        }).await.expect("bounded accepted upload timeout acceptance");
    });
}

#[test]
fn uploaded_avatar_http_preserves_original_results_and_pixels_after_restart() {
    use image::{ExtendedColorType, ImageEncoder as _, codecs::png::PngEncoder};
    use serde_json::{Value, json};

    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let (storage_root, mount, device) = server.files();
            let storage =
                domain::identity::avatar::StorageSettings::new(storage_root.clone(), mount, device)
                    .expect("actual private storage");
            let supplier = Supplier::echoing().await;
            let mut sms = fixture::supplier::Supplier::start().await;
            let application = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                services(8)
                    .expect("settings")
                    .with_avatar_storage(Some(storage.clone())),
                process::Settings::new(Duration::from_secs(5)).expect("stop budget"),
                supplier.client(),
                message_at(sms.address(), sms.authority()),
            )
            .await
            .expect("startup");
            let (base, stop, task) = running(application).await;
            let actor = issued_session_from(&base, &mut sms, "13900000000", "203.0.156.1").await;
            let other = issued_session_from(&base, &mut sms, "13800000000", "203.0.156.2").await;
            let client = http_client();
            let request = |who: &(String, String), method, path: &str, operation: Uuid, source| {
                client
                    .request(method, format!("{base}{path}"))
                    .header("operation-id", operation.to_string())
                    .header("session-id", &who.0)
                    .header("cookie", &who.1)
                    .header("x-real-ip", format!("203.0.156.{source}"))
            };
            let current: Value = request(
                &actor,
                reqwest::Method::GET,
                "/api/v1/accounts/current",
                Uuid::new_v4(),
                3,
            )
            .send()
            .await
            .expect("current account")
            .error_for_status()
            .expect("current success")
            .json()
            .await
            .expect("current JSON");
            let before = current["account"]["avatar"]["revision"]
                .as_str()
                .expect("initial revision");
            let pixels = [
                255, 11, 23, 0, 7, 199, 23, 127, 21, 37, 251, 255, 10, 20, 30, 255,
            ];
            let mut png = Vec::new();
            PngEncoder::new(&mut png)
                .write_image(&pixels, 2, 2, ExtendedColorType::Rgba8)
                .expect("real PNG");
            let operation = Uuid::new_v4();
            let put = |operation, revision: &str, media: &str, bytes: Vec<u8>, source| {
                request(
                    &actor,
                    reqwest::Method::PUT,
                    "/api/v1/accounts/current/avatar",
                    operation,
                    source,
                )
                .header("avatar-revision", revision)
                .header("content-type", media)
                .body(bytes)
            };
            let response = put(operation, before, "image/png", png.clone(), 4)
                .send()
                .await
                .expect("upload");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert!(!response.headers().contains_key("set-cookie"));
            let first: Value = response.json().await.expect("receipt");
            assert_eq!(first["code"], "ok");
            assert_eq!(first["completion"]["id"], operation.to_string());
            assert_eq!(first["completion"]["changed"], true);
            assert_eq!(first["completion"]["revision"], first["avatar"]["revision"]);
            assert_ne!(first["avatar"]["revision"], before);
            assert_eq!(first["avatar"]["width"], 2);
            assert_eq!(first["avatar"]["height"], 2);
            let revision = first["avatar"]["revision"].as_str().expect("new revision");
            let url = url::Url::parse(first["avatar"]["href"].as_str().expect("signed image"))
                .expect("URL");
            let signed = format!("{}?{}", url.path(), url.query().expect("signature"));
            let response = client
                .get(format!("{base}{signed}"))
                .header("x-real-ip", "203.0.156.5")
                .send()
                .await
                .expect("uploaded image");
            assert_eq!(response.status(), 200);
            assert_eq!(response.headers()["content-type"], "image/webp");
            let webp = response.bytes().await.expect("complete file");
            let decoded = image::load_from_memory_with_format(&webp, image::ImageFormat::WebP)
                .expect("real WebP")
                .to_rgba8();
            assert_eq!(decoded.dimensions(), (2, 2));
            assert_eq!(decoded.as_raw(), &pixels);
            let same: Value = put(Uuid::new_v4(), revision, "image/png", png.clone(), 6)
                .send()
                .await
                .expect("unchanged")
                .error_for_status()
                .expect("unchanged success")
                .json()
                .await
                .expect("unchanged receipt");
            assert_eq!(same["completion"]["changed"], false);
            assert_eq!(same["completion"]["revision"], revision);
            let stale = put(Uuid::new_v4(), before, "image/png", png.clone(), 7)
                .send()
                .await
                .expect("stale revision");
            assert_eq!(stale.status(), 409);
            assert_eq!(
                stale.json::<Value>().await.expect("stale outcome"),
                json!({"code":"avatar_changed"})
            );
            let mismatch = put(
                operation,
                before,
                "application/octet-stream",
                png.clone(),
                8,
            )
            .send()
            .await
            .expect("original input mismatch");
            assert_eq!(mismatch.status(), 409);
            assert_eq!(
                mismatch.json::<Value>().await.expect("mismatch outcome"),
                json!({"code":"request_mismatch"})
            );
            let mut next_png = Vec::new();
            PngEncoder::new(&mut next_png)
                .write_image(&[1, 2, 3, 255], 1, 1, ExtendedColorType::Rgba8)
                .expect("next PNG");
            let next: Value = put(Uuid::new_v4(), revision, "image/png", next_png, 9)
                .send()
                .await
                .expect("later change")
                .error_for_status()
                .expect("changed success")
                .json()
                .await
                .expect("later receipt");
            let replay: Value = put(operation, before, "image/png", png, 10)
                .send()
                .await
                .expect("original replay")
                .error_for_status()
                .expect("replay success")
                .json()
                .await
                .expect("replay receipt");
            assert_eq!(replay["completion"], first["completion"]);
            assert_eq!(replay["avatar"]["revision"], next["avatar"]["revision"]);
            let original_path = format!("/api/v1/replacements/{operation}");
            let hidden = request(
                &other,
                reqwest::Method::GET,
                &original_path,
                Uuid::new_v4(),
                11,
            )
            .send()
            .await
            .expect("cross-account original");
            assert_eq!(hidden.status(), 404);
            assert_eq!(
                hidden.json::<Value>().await.expect("hidden result"),
                json!({"code":"result_unavailable"})
            );
            for (response, expected) in [
                (
                    request(
                        &actor,
                        reqwest::Method::GET,
                        &original_path,
                        Uuid::new_v4(),
                        12,
                    )
                    .header("origin", "https://other.example.com"),
                    403,
                ),
                (
                    request(
                        &actor,
                        reqwest::Method::GET,
                        &original_path,
                        Uuid::new_v4(),
                        13,
                    )
                    .header("authorization", "Bearer ignored"),
                    400,
                ),
                (
                    request(
                        &actor,
                        reqwest::Method::GET,
                        &format!("{original_path}?extra=1"),
                        Uuid::new_v4(),
                        16,
                    ),
                    400,
                ),
                (
                    request(
                        &actor,
                        reqwest::Method::GET,
                        &original_path,
                        Uuid::new_v4(),
                        17,
                    )
                    .body("{}"),
                    400,
                ),
                (
                    request(
                        &actor,
                        reqwest::Method::GET,
                        "/api/v1/replacements/invalid",
                        Uuid::new_v4(),
                        18,
                    ),
                    400,
                ),
            ] {
                assert_eq!(
                    response.send().await.expect("website boundary").status(),
                    expected
                );
            }
            stop.send(()).expect("stop");
            let (application, result) = task.await.expect("first server ended");
            result.expect("first drain");
            drop(application);
            let application = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                services(8)
                    .expect("settings")
                    .with_avatar_storage(Some(storage.clone())),
                process::Settings::new(Duration::from_secs(5)).expect("stop budget"),
                supplier.client(),
                message_at(sms.address(), sms.authority()),
            )
            .await
            .expect("restart verifies permanent history and private files");
            let (base, stop, task) = running(application).await;
            let original: Value = client
                .get(format!("{base}{original_path}"))
                .header("operation-id", Uuid::new_v4().to_string())
                .header("session-id", &actor.0)
                .header("cookie", &actor.1)
                .header("x-real-ip", "203.0.156.14")
                .send()
                .await
                .expect("original after restart")
                .error_for_status()
                .expect("known result")
                .json()
                .await
                .expect("original receipt");
            assert_eq!(original["completion"], first["completion"]);
            assert_eq!(original["avatar"]["revision"], next["avatar"]["revision"]);
            let retained = client
                .get(format!("{base}{signed}"))
                .header("x-real-ip", "203.0.156.15")
                .send()
                .await
                .expect("released original file remains readable");
            assert_eq!(retained.status(), 200);
            assert_eq!(retained.bytes().await.expect("retained bytes"), webp);
            stop.send(()).expect("stop");
            let (application, result) = task.await.expect("restarted server ended");
            result.expect("restarted drain");
            drop(application);
            let mut private_files = Vec::new();
            for first in
                std::fs::read_dir(storage_root.join("avatars")).expect("private avatar directory")
            {
                for second in
                    std::fs::read_dir(first.expect("first shard").path()).expect("second shards")
                {
                    for file in std::fs::read_dir(second.expect("second shard").path())
                        .expect("private files")
                    {
                        private_files.push(file.expect("private file").path());
                    }
                }
            }
            let retained_path = private_files
                .into_iter()
                .find(|path| {
                    std::fs::read(path)
                        .expect("actual retained file")
                        .as_slice()
                        == webp.as_ref()
                })
                .expect("original private bytes on disk");
            std::fs::remove_file(retained_path).expect("simulate loss of a retained private file");
            let damaged = Application::start(
                connections(&server, &name, "clerk"),
                keys(0),
                services(8)
                    .expect("settings")
                    .with_avatar_storage(Some(storage)),
                process::Settings::new(Duration::from_secs(5)).expect("stop budget"),
                supplier.client(),
                message_at(sms.address(), sms.authority()),
            )
            .await;
            assert!(
                damaged.is_err(),
                "startup never treats a missing retained private file as legitimate cleanup"
            );
            assert_eq!(supplier.calls.load(Ordering::SeqCst), 2);
            sms.stop().await;
        })
        .await
        .expect("bounded upload and restart acceptance");
    });
}

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
