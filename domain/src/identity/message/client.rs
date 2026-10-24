//! Reusable PNVS configuration, credentials, and HTTP connections.

use std::{
    fmt::{self, Debug, Formatter},
    time::Duration,
};

use reqwest::{ClientBuilder, redirect::Policy};

use crate::identity::message::{
    access_key::AccessKey,
    request::{BuildError, Context, Input, Request},
    settings::Settings,
};

/// The deployment's PNVS configuration, credentials, and reusable connections.
pub struct Client {
    inner: reqwest::Client,
    settings: Settings,
    access_key: AccessKey,
}

impl Client {
    /// Take validated deployment values without loading files or contacting PNVS.
    pub fn new(settings: Settings, access_key: AccessKey) -> Result<Self, reqwest::Error> {
        Ok(Self {
            inner: Self::builder(settings.timeout()).build()?,
            settings,
            access_key,
        })
    }

    /// Use only a loopback TLS supplier with fictional credentials in tests.
    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(
        address: std::net::SocketAddr,
        authority: reqwest::Certificate,
        settings: Settings,
        access_key: AccessKey,
    ) -> Result<Self, reqwest::Error> {
        assert!(address.ip().is_loopback());
        Ok(Self {
            inner: Self::builder(settings.timeout())
                .resolve("dypnsapi.aliyuncs.com", address)
                .add_root_certificate(authority)
                .build()?,
            settings,
            access_key,
        })
    }

    /// Construct and sign one request, bound to this client's connections.
    /// The caller owns sending eligibility and supplies fresh signing context.
    pub fn request(&self, input: Input<'_>, context: Context) -> Result<Request<'_>, BuildError> {
        Request::new(
            &self.inner,
            &self.access_key,
            &self.settings,
            input,
            context,
        )
    }

    fn builder(timeout: Duration) -> ClientBuilder {
        let _ = rustls::crypto::ring::default_provider().install_default();
        reqwest::Client::builder()
            .tls_backend_rustls()
            .https_only(true)
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(timeout.min(Duration::from_secs(3)))
            .timeout(timeout)
    }
}

impl Debug for Client {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Client")
            .field(&"[redacted]")
            .finish()
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{net::SocketAddr, sync::Arc, time::Duration};

    use base64::{Engine, engine::general_purpose::STANDARD};
    use chrono::{DateTime, Utc};
    use reqwest::Certificate;
    use rustls::{ServerConfig, pki_types::PrivatePkcs8KeyDer};
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
        task::JoinHandle,
        time::timeout,
    };
    use tokio_rustls::TlsAcceptor;
    use uuid::Uuid;

    use crate::identity::{
        challenge::Purpose,
        message::{
            access_key::{AccessKey, Id, Secret},
            client::Client,
            request::{Attempt, Context, Error, Input, Request},
            response::{FailureCode, Outcome, Unknown},
            settings::Settings,
        },
    };

    const MESSAGE_ID: &str = "01890f6e-7b80-7000-8000-000000000000";
    const SUCCESS: &str = r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#;
    const REJECTED: &str = r#"{"Code":"biz.FREQUENCY","Success":false}"#;

    // Public test-only self-signed certificate and key, valid through 2126.
    // DNS resolves the production hostname to loopback; TLS still checks that name.
    const CERTIFICATE: &str = "MIIBszCCAVqgAwIBAgIUFpR5zIIPYQwecEdTukZGja0vcdUwCgYIKoZIzj0EAwIwHzEdMBsGA1UEAwwUUE5WUyBsb2NhbCB0ZXN0IG9ubHkwIBcNMjYwOTMwMjMyNjQ2WhgPMjEyNjA5MDYyMzI2NDZaMB8xHTAbBgNVBAMMFFBOVlMgbG9jYWwgdGVzdCBvbmx5MFkwEwYHKoZIzj0CAQYIKoZIzj0DAQcDQgAEKMvAhCICxiDYHcDh4WRRPxrtzTGZLPx8TfT0NvPZ3UwIvZyNA3ejn9MaDhvz2G02LInpJJZAdQBQ9R6g4qabd6NyMHAwHQYDVR0OBBYEFNTvg14P+UH1YcFtJtGJsaJimmyMMB8GA1UdIwQYMBaAFNTvg14P+UH1YcFtJtGJsaJimmyMMCAGA1UdEQQZMBeCFWR5cG5zYXBpLmFsaXl1bmNzLmNvbTAMBgNVHRMBAf8EAjAAMAoGCCqGSM49BAMCA0cAMEQCICB/kjaeTqvsMV4dYF0yG8kRErHmdtkFpNNyT+UdGy8fAiAOHlYhyl7q6T0yL+tvnD5TtHCXINTBDzDBaVw6/G5xfg==";
    const PRIVATE_KEY: &str = "MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgHDBnyXxB+6V2qn6a5pLpl9U0cWWI6a1gUn65SpQddEKhRANCAAQoy8CEIgLGINgdwOHhZFE/Gu3NMZks/HxN9PQ289ndTAi9nI0Dd6Of0xoOG/PYbTYsiekklkB1AFD1HqDippt3";

    fn access_key() -> AccessKey {
        AccessKey::new(
            Id::new("LocalTestAccessKey".into()).expect("fictional id"),
            Secret::new("local-test-secret".into()).expect("fictional secret"),
        )
    }

    fn settings() -> Settings {
        use crate::identity::message::settings::{
            DEFAULT_TIMEOUT_MILLISECONDS, Signature, TemplateCode, Templates,
        };
        Settings::new(
            Signature::new("测试签名".into()).expect("valid signature"),
            Templates::new(
                TemplateCode::new("auth".into()).expect("valid template"),
                TemplateCode::new("change".into()).expect("valid template"),
                TemplateCode::new("binding".into()).expect("valid template"),
            ),
            DEFAULT_TIMEOUT_MILLISECONDS,
        )
        .expect("synthetic settings are valid")
    }

    fn request(client: &Client) -> Request<'_> {
        client
            .request(
                Input {
                    mobile: &"13800138000".parse().expect("synthetic mobile is valid"),
                    code: &"00001234".parse().expect("synthetic code is valid"),
                    message_id: MESSAGE_ID.parse().expect("fixed ID is valid"),
                    valid_seconds: 300,
                    purpose: Purpose::Authentication,
                },
                Context {
                    date: DateTime::parse_from_rfc3339("2026-09-30T00:00:00Z")
                        .expect("fixed date is valid")
                        .with_timezone(&Utc),
                    nonce: Uuid::parse_str("01890f6e-7b80-7000-8000-000000000001")
                        .expect("fixed nonce is valid"),
                },
            )
            .expect("signed test request builds")
    }

    enum Reply {
        Complete(u16, String),
        Truncated,
        Lost,
        HoldHeaders,
        HoldBody,
    }

    fn local_client(address: SocketAddr, budget: Duration) -> (Client, TlsAcceptor) {
        let certificate = STANDARD.decode(CERTIFICATE).expect("test DER decodes");
        let client = Client {
            inner: Client::builder(budget)
                .resolve("dypnsapi.aliyuncs.com", address)
                .tls_certs_only([
                    Certificate::from_der(&certificate).expect("test certificate parses")
                ])
                .build()
                .expect("isolated client builds"),
            settings: settings(),
            access_key: access_key(),
        };
        let server = ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(
                vec![certificate.into()],
                PrivatePkcs8KeyDer::from(STANDARD.decode(PRIVATE_KEY).expect("test key decodes"))
                    .into(),
            )
            .expect("test TLS config builds");
        (client, TlsAcceptor::from(Arc::new(server)))
    }

    async fn serve(reply: Reply, budget: Duration) -> (Client, JoinHandle<Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener binds");
        let (client, acceptor) = local_client(
            listener.local_addr().expect("listener has an address"),
            budget,
        );
        let server = tokio::spawn(async move {
            timeout(Duration::from_secs(3), async move {
                let (socket, _) = listener.accept().await.expect("first connection arrives");
                let mut socket = acceptor
                    .accept(socket)
                    .await
                    .expect("TLS validates the fixed hostname");
                let mut request = Vec::new();
                let mut buffer = [0; 4096];
                while !request.windows(4).any(|part| part == b"\r\n\r\n") {
                    let size = socket.read(&mut buffer).await.expect("request reads");
                    assert!(size > 0, "complete request headers arrive");
                    request.extend_from_slice(&buffer[..size]);
                }
                match reply {
                    Reply::Complete(status, body) => {
                        let response = format!(
                            "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nLocation: https://dypnsapi.aliyuncs.com/redirected\r\nConnection: close\r\n\r\n{body}",
                            body.len()
                        );
                        socket.write_all(response.as_bytes()).await.expect("reply writes");
                    }
                    Reply::Truncated => {
                        socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                            .await.expect("truncated reply writes");
                    }
                    Reply::Lost => {}
                    Reply::HoldHeaders | Reply::HoldBody => {
                        if matches!(reply, Reply::HoldBody) {
                            socket.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n{")
                                .await.expect("partial body writes");
                        }
                        // Synchronize with the client ending its timed-out attempt.
                        let _ = socket.read(&mut buffer).await;
                    }
                }
                drop(socket);
                assert!(
                    timeout(Duration::from_millis(100), listener.accept()).await.is_err(),
                    "one send must cause exactly one connection, with no retry or redirect"
                );
                request
            })
            .await
            .expect("test server finishes within its deadline")
        });
        (client, server)
    }

    #[tokio::test]
    async fn reuses_connections_without_mixing_messages() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("listener binds");
        let (client, acceptor) = local_client(
            listener.local_addr().expect("listener has an address"),
            Duration::from_secs(1),
        );
        let second_id = "01890f6e-7b80-7000-8000-000000000002";
        let server = tokio::spawn(async move {
            timeout(Duration::from_secs(3), async move {
                let (socket, _) = listener.accept().await.expect("connection arrives");
                let mut socket = acceptor.accept(socket).await.expect("TLS succeeds");
                for (id, template, code) in [
                    (MESSAGE_ID, "auth", "00001234"),
                    (second_id, "change", "98765432"),
                ] {
                    let mut bytes = Vec::new();
                    let mut buffer = [0; 4096];
                    while !bytes.windows(4).any(|part| part == b"\r\n\r\n") {
                        let size = socket.read(&mut buffer).await.expect("request reads");
                        assert!(size > 0);
                        bytes.extend_from_slice(&buffer[..size]);
                    }
                    let wire = String::from_utf8(bytes).expect("request is ASCII");
                    assert!(wire.starts_with("POST /?"));
                    assert!(wire.contains(&format!("OutId={id}&")));
                    assert!(wire.contains(&format!("TemplateCode={template}&")));
                    assert!(wire.contains(&format!("TemplateParam=%7B%22code%22%3A%22{code}%22")));
                    assert!(wire.ends_with("\r\n\r\n"));
                    let body = SUCCESS.replace(MESSAGE_ID, id);
                    let response = format!(
                        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n{body}",
                        body.len()
                    );
                    socket
                        .write_all(response.as_bytes())
                        .await
                        .expect("reply writes");
                }
                assert!(
                    timeout(Duration::from_millis(100), listener.accept())
                        .await
                        .is_err()
                );
            })
            .await
            .expect("both requests reuse the same connection within the deadline");
        });
        assert_eq!(format!("{client:?}"), "Client(\"[redacted]\")");
        assert!(matches!(
            request(&client)
                .execute()
                .await
                .expect("first response")
                .outcome(),
            Outcome::Accepted(_)
        ));
        let second = client
            .request(
                Input {
                    mobile: &"13800138000".parse().expect("valid mobile"),
                    code: &"98765432".parse().expect("valid code"),
                    message_id: second_id.parse().expect("valid ID"),
                    valid_seconds: 300,
                    purpose: Purpose::MobileChange,
                },
                Context {
                    date: DateTime::parse_from_rfc3339("2026-09-30T00:01:00Z")
                        .expect("valid date")
                        .with_timezone(&Utc),
                    nonce: Uuid::parse_str("01890f6e-7b80-7000-8000-000000000003")
                        .expect("valid nonce"),
                },
            )
            .expect("second request builds");
        assert!(matches!(
            second.execute().await.expect("second response").outcome(),
            Outcome::Accepted(_)
        ));
        server
            .await
            .expect("server verified connection reuse and per-message values");
    }

    async fn attempt(reply: Reply) -> Attempt {
        execute(reply).await.expect("HTTP response decoded")
    }

    async fn execute(reply: Reply) -> Result<Attempt, Error> {
        let (client, server) = serve(reply, Duration::from_secs(1)).await;
        let attempt = timeout(Duration::from_secs(2), request(&client).execute())
            .await
            .expect("one send is bounded");
        server.await.expect("server verifies exactly one call");
        attempt
    }

    #[tokio::test]
    async fn sends_the_original_signed_bytes_over_verified_https() {
        let (client, server) =
            serve(Reply::Complete(200, SUCCESS.into()), Duration::from_secs(1)).await;
        let request = request(&client);
        // Fixed wire expectations independently derived with Python urllib/hashlib/hmac.
        let expected_line = "POST /?AutoRetry=0&CodeLength=8&CodeType=1&CountryCode=86&DuplicatePolicy=1&Interval=60&OutId=01890f6e-7b80-7000-8000-000000000000&PhoneNumber=13800138000&ReturnVerifyCode=false&SignName=%E6%B5%8B%E8%AF%95%E7%AD%BE%E5%90%8D&TemplateCode=auth&TemplateParam=%7B%22code%22%3A%2200001234%22%2C%22min%22%3A%225%22%7D&ValidTime=300 HTTP/1.1";
        let headers = [
            ("host", "dypnsapi.aliyuncs.com"),
            ("accept", "application/json"),
            ("x-acs-action", "SendSmsVerifyCode"),
            ("x-acs-version", "2017-05-25"),
            ("x-acs-date", "2026-09-30T00:00:00Z"),
            (
                "x-acs-signature-nonce",
                "01890f6e-7b80-7000-8000-000000000001",
            ),
            (
                "x-acs-content-sha256",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
            ),
            (
                "authorization",
                "ACS3-HMAC-SHA256 Credential=LocalTestAccessKey,SignedHeaders=host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-version,Signature=195f5f2dd7f87baaa3d66de31e42396248c835e441b5ce13427261125ef56924",
            ),
        ];
        let result = request.execute().await.expect("response headers arrive");
        assert!(matches!(result.outcome(), Outcome::Accepted(accepted)
            if accepted.biz_id() == "supplier-business-id" && accepted.request_id().to_string() == "a3671ccf-0102-4c8e-8797-a3678e091d09"));
        assert_eq!(result.status(), reqwest::StatusCode::OK);
        let wire =
            String::from_utf8(server.await.expect("server finishes")).expect("wire is UTF-8");
        let (head, body) = wire.split_once("\r\n\r\n").expect("complete HTTP headers");
        assert_eq!(head.lines().next(), Some(expected_line));
        assert!(body.is_empty());
        for (name, value) in headers {
            let values: Vec<_> = head
                .lines()
                .skip(1)
                .filter_map(|line| {
                    let (key, value) = line.split_once(':')?;
                    key.eq_ignore_ascii_case(name).then_some(value.trim())
                })
                .collect();
            assert_eq!(values, [value]);
        }
    }

    #[tokio::test]
    async fn accepts_only_successful_http_statuses() {
        for status in [201, 202, 299] {
            assert!(matches!(
                attempt(Reply::Complete(status, SUCCESS.into()))
                    .await
                    .outcome(),
                Outcome::Accepted(_)
            ));
        }
        for status in [401, 403, 429] {
            assert!(matches!(
                execute(Reply::Complete(status, SUCCESS.into())).await,
                Err(Error::Decode { .. })
            ));
        }
    }

    #[tokio::test]
    async fn recognizes_known_rejections_on_2xx_and_400() {
        for code in [
            "ILLEGAL_MOBILE",
            "isv.INVALID_PARAMETERS",
            "biz.FREQUENCY",
            "FUNCTION_NOT_OPENED",
            "isv.BUSINESS_LIMIT_CONTROL",
        ] {
            for status in [200, 201, 400, 401, 403, 429] {
                let body = format!(r#"{{"Success":false,"Code":"{code}"}}"#);
                let result = attempt(Reply::Complete(status, body)).await;
                assert_eq!(result.status().as_u16(), status);
                match status {
                    200 | 201 | 400 => assert!(
                        matches!(result.outcome(), Outcome::Rejected(actual) if *actual == code.parse::<FailureCode>().expect("known code"))
                    ),
                    _ => assert!(
                        matches!(result.outcome(), Outcome::Unknown(Unknown::UnexpectedStatus(actual)) if *actual == code.parse::<FailureCode>().expect("known code"))
                    ),
                }
            }
        }
    }

    #[tokio::test]
    async fn retains_unknown_client_error_codes_without_disclosing_them() {
        let marker = "13800138000-00001234-local-test-secret";
        for status in [200, 201, 400, 401, 403, 429, 499] {
            let body = format!(r#"{{"Success":false,"Code":"{marker}","Message":"{marker}"}}"#);
            let result = attempt(Reply::Complete(status, body)).await;
            assert_eq!(result.status().as_u16(), status);
            assert!(!format!("{result:?}").contains(marker));
            assert!(
                matches!(result.outcome(), Outcome::Unknown(Unknown::UnrecognizedCode(code)) if code == marker)
            );
            let result = attempt(Reply::Complete(
                status,
                r#"{"Code":"InvalidAccessKeyId"}"#.into(),
            ))
            .await;
            assert!(
                matches!(result.outcome(), Outcome::Unknown(Unknown::UnrecognizedCode(code)) if code == "InvalidAccessKeyId")
            );
            for body in ["", marker] {
                let result = execute(Reply::Complete(status, body.into())).await;
                assert!(matches!(&result, Err(Error::Decode { .. })));
                assert!(!format!("{result:?}").contains(marker));
            }
        }
    }

    #[tokio::test]
    async fn decodes_public_errors_without_success_flag() {
        for (status, code) in [
            (400, "SignatureDoesNotMatch"),
            (400, "MissingPhoneNumber"),
            (400, "InvalidTimeStamp.Expired"),
            (400, "InvalidValidTime"),
            (404, "InvalidAccessKeyId.NotFound"),
        ] {
            let body = format!(
                r#"{{"RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09","Message":"supplier failure","Recommend":"https://example.invalid/","HostId":"dypnsapi.aliyuncs.com","Code":"{code}"}}"#
            );
            let result = attempt(Reply::Complete(status, body)).await;
            assert_eq!(result.status().as_u16(), status);
            assert!(
                matches!(result.outcome(), Outcome::Unknown(Unknown::UnrecognizedCode(actual)) if actual == code)
            );
        }
    }

    #[tokio::test]
    async fn retains_internal_error_on_200_as_unknown() {
        let body = r#"{"Message":"System Internal Error","Code":"InternalError","Success":false}"#;
        let result = attempt(Reply::Complete(200, body.into())).await;
        assert!(
            matches!(result.outcome(), Outcome::Unknown(Unknown::UnrecognizedCode(code)) if code == "InternalError")
        );
    }

    #[tokio::test]
    async fn abnormal_statuses_never_establish_a_business_result_or_follow_redirects() {
        for status in [301, 302, 307, 308, 500, 503] {
            for body in [SUCCESS, REJECTED] {
                let result = attempt(Reply::Complete(status, body.into())).await;
                assert_eq!(result.status().as_u16(), status);
                assert!(matches!(
                    result.outcome(),
                    Outcome::Unknown(Unknown::HttpStatus)
                ));
            }
        }
    }

    #[tokio::test]
    async fn mismatched_or_inconsistent_content_is_unknown() {
        let mismatch = SUCCESS.replace(MESSAGE_ID, "01890f6e-7b80-7000-8000-000000000002");
        assert!(matches!(
            attempt(Reply::Complete(200, mismatch)).await.outcome(),
            Outcome::Unknown(Unknown::OutIdMismatch)
        ));
        let empty_id = SUCCESS.replace("supplier-business-id", " ");
        assert!(matches!(
            attempt(Reply::Complete(200, empty_id)).await.outcome(),
            Outcome::Unknown(Unknown::Inconsistent)
        ));
        for (status, body) in [
            (204, ""),
            (200, "invalid JSON 00001234"),
            (200, r#"{"Code":"OK","Success":true}"#),
            (200, r#"{"Code":"OK","Success":false}"#),
            (400, r#"{"Code":"biz.FREQUENCY","Success":true}"#),
            (400, r#"{"Success":"13800138000"}"#),
            (400, SUCCESS),
            (400, r#"{"Code":"OK","Success":false}"#),
        ] {
            let result = execute(Reply::Complete(status, body.into()))
                .await
                .expect_err("invalid JSON response");
            assert!(matches!(&result, Error::Decode { .. }));
            assert!(!format!("{result:?}").contains("13800138000"));
            assert!(!format!("{result:?}").contains("00001234"));
        }
    }

    #[tokio::test]
    async fn response_loss_and_truncation_are_unknown_without_resending() {
        let (client, server) = serve(Reply::Lost, Duration::from_secs(1)).await;
        let lost = timeout(Duration::from_secs(2), request(&client).execute())
            .await
            .expect("lost response is bounded")
            .expect_err("no response headers arrived");
        let Error::Reqwest(source) = &lost else {
            panic!("request transport error");
        };
        assert!(source.url().is_none());
        assert!(!source.is_timeout());
        for diagnostic in [format!("{lost:?}"), lost.to_string()] {
            for forbidden in [
                "13800138000",
                "00001234",
                "local-test-secret",
                "https://dypnsapi.aliyuncs.com",
                "TemplateParam",
                "Authorization",
            ] {
                assert!(
                    !diagnostic.contains(forbidden),
                    "diagnostic contains forbidden marker {forbidden}"
                );
            }
        }
        server.await.expect("response loss does not resend");
        let truncated = execute(Reply::Truncated)
            .await
            .expect_err("truncated body fails");
        let Error::Read { status, source } = &truncated else {
            panic!("body read error");
        };
        assert_eq!(status.as_u16(), 200);
        assert!(!source.is_timeout());
        assert!(source.url().is_none());
        assert!(std::error::Error::source(&truncated).is_some());
        for forbidden in [
            "13800138000",
            "00001234",
            "local-test-secret",
            "TemplateParam",
            "Authorization",
        ] {
            assert!(!format!("{truncated:?}").contains(forbidden));
        }
    }

    #[tokio::test]
    async fn total_budget_covers_headers_and_body_without_resending() {
        for reply in [Reply::HoldHeaders, Reply::HoldBody] {
            let budget = Duration::from_millis(100);
            let (client, server) = serve(reply, budget).await;
            let started = std::time::Instant::now();
            let result = timeout(Duration::from_secs(1), request(&client).execute())
                .await
                .expect("total budget enforced");
            let error = match result.expect_err("headers or body timed out") {
                Error::Reqwest(error) | Error::Read { source: error, .. } => error,
                _ => panic!("transport timeout"),
            };
            assert!(error.is_timeout());
            assert!(error.url().is_none());
            assert!(started.elapsed() >= budget);
            server.await.expect("timeout does not resend");
        }
    }

    #[tokio::test]
    async fn connection_budget_includes_tls_and_is_capped_at_three_seconds() {
        for budget in [Duration::from_secs(1), Duration::from_secs(10)] {
            let listener = TcpListener::bind("127.0.0.1:0")
                .await
                .expect("loopback listener binds");
            let address = listener.local_addr().expect("listener has an address");
            let client = Client {
                inner: Client::builder(budget)
                    .resolve("dypnsapi.aliyuncs.com", address)
                    .build()
                    .expect("client builds"),
                settings: settings(),
                access_key: access_key(),
            };
            let server = tokio::spawn(async move {
                timeout(Duration::from_secs(5), async move {
                    let (mut socket, _) = listener.accept().await.expect("TLS connection arrives");
                    let mut hello = Vec::new();
                    // Withhold the TLS reply until the client closes on timeout.
                    socket.read_to_end(&mut hello).await.expect("client closes");
                    assert!(!hello.is_empty(), "client sends a TLS hello");
                    assert!(
                        timeout(Duration::from_millis(100), listener.accept())
                            .await
                            .is_err()
                    );
                })
                .await
                .expect("stalled TLS connection is bounded");
            });
            let started = std::time::Instant::now();
            let result = timeout(Duration::from_secs(5), request(&client).execute())
                .await
                .expect("connection budget is shorter than ten seconds");
            let Error::Reqwest(error) = result.expect_err("TLS handshake timed out") else {
                panic!("TLS transport timeout");
            };
            assert!(error.is_timeout());
            assert!(error.url().is_none());
            assert!(started.elapsed() >= budget.min(Duration::from_secs(3)));
            server.await.expect("connection timeout does not retry");
        }
    }

    #[tokio::test]
    async fn diagnostics_hide_arbitrary_supplier_text_and_request_material() {
        let marker = "13800138000-00001234-local-test-secret";
        for (status, body) in [
            (400, format!(r#"{{"Success":false,"Code":"{marker}"}}"#)),
            (200, SUCCESS.replace("supplier-business-id", marker)),
        ] {
            let result = attempt(Reply::Complete(status, body)).await;
            let debug = format!("{result:?}");
            for forbidden in [
                marker,
                "13800138000",
                "00001234",
                "local-test-secret",
                "Authorization",
                "TemplateParam",
                "https://",
            ] {
                assert!(!debug.contains(forbidden));
            }
        }
        assert!(
            matches!(attempt(Reply::Complete(400, r#"{"Success":false,"Code":"NEW_CODE"}"#.into())).await.outcome(), Outcome::Unknown(Unknown::UnrecognizedCode(code)) if code == "NEW_CODE")
        );
    }
}
