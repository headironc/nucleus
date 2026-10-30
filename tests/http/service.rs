//! The delivered `nucleus` binary served from its real external configuration.
//!
//! Capability modules start the application in-process. This module instead runs the built
//! executable with exported settings, a `CREDENTIALS_DIRECTORY` and a systemd notification
//! socket, so CLI parsing, settings and credential loading, diagnostics and `process::run`
//! are covered end to end.

use std::{
    fs::{self, File},
    os::unix::fs::{MetadataExt, PermissionsExt},
    path::Path,
    process::Stdio,
    time::Duration,
};

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde_json::{Value, json};
use tokio::{net::UnixDatagram, process::Command, runtime::Runtime, time::timeout};
use uuid::Uuid;

use crate::database::{database, server};

fn credentials(directory: &Path) {
    let write = |name: &str, value: Value| {
        fs::write(
            directory.join(name),
            serde_json::to_vec(&value).expect("credential JSON"),
        )
        .expect("fictional credential file");
    };
    // The same fictional root versions that `application::keys(0)` registers.
    let keys: Vec<Value> = (0..2u8)
        .map(|id| json!({"id": id, "material": URL_SAFE_NO_PAD.encode([1 + id; 32])}))
        .collect();
    write(
        "keys.json",
        json!({"version": 1, "active": 0, "keys": keys}),
    );
    write(
        "database.json",
        json!({"version": 1, "password": fixture::PASSWORD}),
    );
    write(
        "challenge.json",
        json!({"version": 1, "secret": "fictional-secret"}),
    );
    write(
        "message.json",
        json!({"version": 1, "id": "FictionalAccessKey", "secret": "fictional-secret"}),
    );
}

/// The one loopback TCP port the process listens on, found through its socket descriptors.
fn listening_port(pid: u32) -> u16 {
    let sockets: Vec<String> = fs::read_dir(format!("/proc/{pid}/fd"))
        .expect("service descriptors")
        .filter_map(|entry| fs::read_link(entry.ok()?.path()).ok())
        .filter_map(|target| {
            let target = target.to_str()?;
            Some(
                target
                    .strip_prefix("socket:[")?
                    .strip_suffix(']')?
                    .to_owned(),
            )
        })
        .collect();
    let table = fs::read_to_string(format!("/proc/{pid}/net/tcp")).expect("TCP socket table");
    let ports: Vec<u16> = table
        .lines()
        .skip(1)
        .filter_map(|line| {
            let fields: Vec<&str> = line.split_whitespace().collect();
            // Fields: index, local address, remote address, state, ..., inode at index 9.
            let (address, port) = fields.get(1)?.split_once(':')?;
            let state = fields.get(3)?;
            let inode = fields.get(9)?;
            let listening = address == "0100007F"
                && *state == "0A"
                && sockets.iter().any(|socket| socket == inode);
            listening.then(|| u16::from_str_radix(port, 16).ok())?
        })
        .collect();
    assert_eq!(ports.len(), 1, "exactly one loopback listener: {ports:?}");
    ports[0]
}

#[test]
fn delivered_binary_serves_from_external_configuration_and_stops_on_sigterm() {
    let server = server();
    Runtime::new().expect("runtime").block_on(async {
        timeout(Duration::from_secs(60), async {
            let name = database(&server).await;
            let directory = tempfile::tempdir().expect("isolated service directory");
            let secrets = directory.path().join("credentials");
            fs::create_dir(&secrets).expect("credentials directory");
            credentials(&secrets);
            let socket = directory.path().join("notify");
            let storage = directory.path().join("storage");
            fs::create_dir(&storage).expect("storage root");
            fs::set_permissions(&storage, fs::Permissions::from_mode(0o700))
                .expect("private storage root");
            let device = fs::metadata(&storage).expect("storage device").dev();
            let mut mount = storage.clone();
            while let Some(parent) = mount.parent() {
                if fs::metadata(parent).expect("mount ancestor").dev() != device { break; }
                mount = parent.to_path_buf();
            }
            let notifications = UnixDatagram::bind(&socket).expect("systemd notification socket");
            let stderr = directory.path().join("stderr");
            let settings = [
                ("NUCLEUS_CHALLENGE__HOSTNAMES", r#"["example.com"]"#.to_owned()),
                ("NUCLEUS_CHALLENGE__SITE_KEY", "fixture-site-key".to_owned()),
                ("NUCLEUS_MESSAGE__SIGNATURE", "测试".to_owned()),
                ("NUCLEUS_MESSAGE__TEMPLATE__AUTHENTICATION", "auth".to_owned()),
                ("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_CHANGE", "change".to_owned()),
                ("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_BINDING", "bind".to_owned()),
                ("NUCLEUS_DATABASE__HOST", "127.0.0.1".to_owned()),
                (
                    "NUCLEUS_DATABASE__AUTHORITY",
                    server.authority().to_str().expect("UTF-8 authority path").to_owned(),
                ),
                ("NUCLEUS_DATABASE__PORT", server.port().to_string()),
                ("NUCLEUS_DATABASE__NAME", name.clone()),
                ("NUCLEUS_DATABASE__USER", "clerk".to_owned()),
                ("NUCLEUS_DATABASE__CAPACITY", "2".to_owned()),
                ("NUCLEUS_HTTP__LISTEN", "127.0.0.1:0".to_owned()),
                ("NUCLEUS_HTTP__ORIGIN", "https://example.com".to_owned()),
                ("NUCLEUS_HTTP__TRUSTED_PROXIES", r#"["127.0.0.1/32"]"#.to_owned()),
                ("NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY", "8".to_owned()),
                ("NUCLEUS_IDENTITY__AVATAR__ORIGIN", "https://avatars.example.com".to_owned()),
                ("NUCLEUS_STORAGE__ROOT", storage.to_str().expect("UTF-8 storage root").to_owned()),
                ("NUCLEUS_STORAGE__MOUNT", mount.to_str().expect("UTF-8 mount").to_owned()),
                ("NUCLEUS_STORAGE__DEVICE", device.to_string()),
                ("NUCLEUS_WECHAT__WEBSITE__APP_ID", "website-fixture".to_owned()),
                ("NUCLEUS_PROCESS__STOP_SECONDS", "10".to_owned()),
            ];
            let mut child = Command::new(env!("CARGO_BIN_EXE_nucleus"))
                .env_clear()
                .envs(settings)
                .env("NUCLEUS_WORKFORCE__APPLICATION__BACKGROUND_PATH", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/avatars/0-cat.webp"))
                .env("NUCLEUS_WORKFORCE__APPLICATION__BACKGROUND_MEDIA_TYPE", "image/webp")
                .env("NUCLEUS_WORKFORCE__APPLICATION__FONT_PATH", std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("domain/tests/fixtures/fonts/SourceSans3-Regular.ttf"))
                .env("NUCLEUS_WORKFORCE__APPLICATION__TEMPLATE", serde_json::to_string(&domain::workforce::application::fixture_template()).expect("fixture template"))
                .env("NUCLEUS_WORKFORCE__APPLICATION__PICKUPS", r#"["Fixture reception"]"#)
                .env("CREDENTIALS_DIRECTORY", &secrets)
                .env("NOTIFY_SOCKET", &socket)
                .current_dir(directory.path())
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(File::create(&stderr).expect("service diagnostics capture"))
                .kill_on_drop(true)
                .spawn()
                .expect("start the delivered binary");
            let diagnostics = || fs::read_to_string(&stderr).unwrap_or_default();
            let mut buffer = [0; 256];
            let received = tokio::select! {
                result = notifications.recv(&mut buffer) => result.expect("readiness datagram"),
                status = child.wait() => panic!("service exited before readiness: {status:?}\n{}", diagnostics()),
                () = tokio::time::sleep(Duration::from_secs(30)) => panic!("service never announced readiness\n{}", diagnostics()),
            };
            assert_eq!(std::str::from_utf8(&buffer[..received]).expect("notification text").trim_end(), "READY=1", "{}", diagnostics());

            // The service binds an ephemeral port itself; readiness follows the bind.
            let port = listening_port(child.id().expect("running service"));
            let _ = rustls::crypto::ring::default_provider().install_default();
            let client = reqwest::Client::builder()
                .no_proxy()
                .timeout(Duration::from_secs(5))
                .build()
                .expect("HTTP client");
            let base = format!("http://127.0.0.1:{port}");
            let get = |path: &str| {
                client
                    .get(format!("{base}{path}"))
                    .header("operation-id", Uuid::new_v4().to_string())
                    .header("x-real-ip", "192.0.2.10")
            };
            for (path, body) in [
                ("/health/live", json!({"code":"ok","live":true})),
                ("/health/ready", json!({"code":"ok","ready":true})),
            ] {
                let response = get(path).send().await.expect("health probe");
                assert_eq!(response.status(), 200, "{path}");
                assert_eq!(response.json::<Value>().await.expect("health JSON"), body);
            }
            let response = get("/api/v1/accounts/current")
                .header("origin", "https://example.com")
                .send()
                .await
                .expect("anonymous current account");
            assert_eq!(response.status(), 401);
            assert_eq!(response.headers()["cache-control"], "no-store");
            assert_eq!(
                response.json::<Value>().await.expect("failure JSON"),
                json!({"code":"authentication_invalid"})
            );

            let pid = child.id().expect("running service").to_string();
            let signal = Command::new("kill")
                .args(["-TERM", &pid])
                .status()
                .await
                .expect("send SIGTERM");
            assert!(signal.success());
            let status = timeout(Duration::from_secs(15), child.wait())
                .await
                .expect("bounded drain after SIGTERM")
                .expect("service exit status");
            let text = diagnostics();
            assert!(status.success(), "{status:?}\n{text}");
            let events: Vec<Value> = text
                .lines()
                .map(|line| serde_json::from_str(line).expect("JSON diagnostic event"))
                .collect();
            assert!(!events.iter().any(|event| event["level"] == "ERROR"), "{text}");
            assert!(
                events.iter().any(|event| event["fields"]["stage"] == "stopped"),
                "the application reports a completed drain: {text}"
            );
            assert!(get("/health/live").send().await.is_err(), "listener closed after exit");
        })
        .await
        .expect("bounded delivered-binary lifecycle");
    });
}
