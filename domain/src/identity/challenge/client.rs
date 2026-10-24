//! Reusable Siteverify settings, credential, and single-attempt transport.

use std::{
    fmt::{self, Formatter},
    time::Duration,
};

use derive_more::AsRef;
use reqwest::{StatusCode, redirect::Policy};
use serde::Serialize;
use thiserror::Error;
use url::Url;
use zeroize::Zeroizing;

use crate::identity::{
    challenge::{
        Id, Purpose, Token,
        credentials::Secret,
        response::{Context, Response},
        settings::Settings,
    },
    operation,
};

const SITEVERIFY_ADDRESS: &str = "https://challenges.cloudflare.com/turnstile/v0/siteverify";
const RESPONSE_LIMIT: usize = 64 * 1024;

/// A single-attempt Siteverify transport. The caller owns admission and retries.
pub struct Client {
    inner: reqwest::Client,
    address: Url,
    settings: Settings,
    secret: Secret,
}

impl Client {
    /// Build the fixed-address client without redirects or automatic retries.
    pub fn new(settings: Settings, secret: Secret) -> Result<Self, BuildError> {
        Ok(Self::with_address(
            Url::parse(SITEVERIFY_ADDRESS)?,
            settings,
            secret,
        )?)
    }

    /// Use a loopback HTTP fixture in a consuming package's tests.
    #[cfg(feature = "test-support")]
    pub fn for_test(port: u16, settings: Settings, secret: Secret) -> Result<Self, BuildError> {
        Ok(Self::with_address(
            Url::parse(&format!("http://127.0.0.1:{port}/siteverify"))?,
            settings,
            secret,
        )?)
    }

    pub(in crate::identity) fn with_address(
        address: Url,
        settings: Settings,
        secret: Secret,
    ) -> Result<Self, reqwest::Error> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let client = reqwest::Client::builder()
            .tls_backend_rustls()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .build()?;
        Ok(Self {
            inner: client,
            address,
            settings,
            secret,
        })
    }

    /// Submit one already-admitted validation within the remaining call budget.
    pub async fn verify(
        &self,
        token: &Token,
        challenge_id: Id,
        purpose: Purpose,
        operation_id: operation::Id,
        remaining: Duration,
    ) -> Result<Context, Error> {
        if remaining.is_zero() {
            return Err(Error::Expired);
        }

        let idempotency_key = challenge_id.to_string();
        let request = VerifyRequest {
            secret: self.secret.as_ref(),
            response: token.as_ref(),
            idempotency_key: &idempotency_key,
        };
        let mut response = self
            .inner
            .post(self.address.clone())
            .timeout(remaining.min(Duration::from_secs(10)))
            .json(&request)
            .send()
            .await?;
        let status = response.status();
        let mut body = Zeroizing::new(Vec::new());
        loop {
            let option = match response.chunk().await {
                Ok(option) => option,
                Err(source) if !status.is_success() => {
                    return Err(Error::StatusRead {
                        status,
                        source: source.without_url(),
                    });
                }
                Err(source) => return Err(source.into()),
            };
            let Some(chunk) = option else { break };
            if chunk.len() > RESPONSE_LIMIT - body.len() {
                return Err(Error::TooLarge);
            }
            body.extend_from_slice(&chunk);
        }
        if !status.is_success() {
            let body = StatusBody(Zeroizing::new(String::from_utf8_lossy(&body).into_owned()));
            return Err(Error::Status { status, body });
        }
        let response: Response = serde_json::from_slice(&body)?;
        Ok(response.check(purpose, operation_id, self.settings.hostnames()))
    }
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Client")
            .field(&"[redacted]")
            .finish()
    }
}

/// Why the Siteverify HTTP client cannot be constructed at startup.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("invalid siteverify address")]
    Address(#[from] url::ParseError),
    #[error("siteverify HTTP client initialization failed")]
    Reqwest(#[from] reqwest::Error),
}

#[derive(Serialize)]
struct VerifyRequest<'a> {
    secret: &'a str,
    response: &'a str,
    idempotency_key: &'a str,
}

/// The retained text of an unsuccessful Siteverify HTTP response.
#[derive(AsRef)]
#[as_ref(str)]
pub struct StatusBody(Zeroizing<String>);

impl fmt::Debug for StatusBody {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("StatusBody")
            .field(&"[redacted]")
            .finish()
    }
}

/// Why one Siteverify HTTP attempt has no reliable decoded result.
#[derive(Error)]
pub enum Error {
    #[error("siteverify call budget is exhausted")]
    Expired,
    #[error("siteverify attempt exceeded the remaining budget")]
    Timeout(#[from] tokio::time::error::Elapsed),
    #[error("siteverify response exceeds its byte limit")]
    TooLarge,
    #[error("siteverify response cannot be decoded")]
    Decode(#[from] serde_json::Error),
    #[error("siteverify HTTP request or response failed")]
    Reqwest(#[source] reqwest::Error),
    #[error("siteverify returned HTTP {status}")]
    Status {
        status: StatusCode,
        body: StatusBody,
    },
    #[error("siteverify returned HTTP {status} and its body could not be read")]
    StatusRead {
        status: StatusCode,
        #[source]
        source: reqwest::Error,
    },
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Reqwest(error.without_url())
    }
}

impl Error {
    pub(in crate::identity) fn retryable(&self) -> bool {
        match self {
            Self::Reqwest(source) => source.is_connect() || source.is_timeout(),
            Self::Timeout(_) => true,
            Self::Status { status, .. } | Self::StatusRead { status, .. } => matches!(
                *status,
                StatusCode::INTERNAL_SERVER_ERROR
                    | StatusCode::BAD_GATEWAY
                    | StatusCode::SERVICE_UNAVAILABLE
                    | StatusCode::GATEWAY_TIMEOUT
            ),
            Self::Expired | Self::TooLarge | Self::Decode(_) => false,
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Expired => formatter.write_str("Expired"),
            Self::Timeout(_) => formatter.write_str("Timeout"),
            Self::TooLarge => formatter.write_str("TooLarge"),
            Self::Decode(_) => formatter
                .debug_tuple("Decode")
                .field(&"[redacted]")
                .finish(),
            Self::Reqwest(source) if source.is_decode() => formatter
                .debug_tuple("Reqwest")
                .field(&"[redacted]")
                .finish(),
            Self::Reqwest(source) => formatter.debug_tuple("Reqwest").field(source).finish(),
            Self::Status { status, body } => formatter
                .debug_struct("Status")
                .field("status", status)
                .field("body", body)
                .finish(),
            Self::StatusRead { status, source } => formatter
                .debug_struct("StatusRead")
                .field("status", status)
                .field("source", source)
                .finish(),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::time::Duration;

    use nonempty::NonEmpty;
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        net::TcpListener,
    };
    use url::Url;

    use crate::identity::challenge::{
        Id, Purpose, Token,
        client::{Client, Error},
        credentials::Secret,
        response::{Code, Context},
        settings::{AllowedHostnames, Settings},
    };
    const OPERATION_ID: &str = "01890f6e-7b80-7000-8000-000000000000";

    fn allowed_hostnames() -> AllowedHostnames {
        AllowedHostnames::new(NonEmpty::new("example.com".to_owned()))
            .expect("fixed hostname allowlist is nonempty")
    }

    fn server_secret() -> Secret {
        "test-secret".parse().expect("fixed server secret is valid")
    }

    async fn serve(
        status: u16,
        body: String,
        declared_length: Option<usize>,
        calls: usize,
    ) -> (Client, tokio::task::JoinHandle<Vec<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener binds");
        let address = listener
            .local_addr()
            .expect("local listener has an address");
        let address = Url::parse(&format!("http://{address}/turnstile/v0/siteverify"))
            .expect("local test URL parses");
        let client =
            Client::with_address(address, Settings::new(allowed_hostnames()), server_secret())
                .expect("test client builds");
        let server = tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(3), async move {
            let mut requests = Vec::new();
            for _ in 0..calls {
            let (mut socket, _) = listener.accept().await.expect("one request arrives");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            let header_end = loop {
                let count = socket
                    .read(&mut buffer)
                    .await
                    .expect("request read succeeds");
                assert!(count > 0, "request has complete headers");
                request.extend_from_slice(&buffer[..count]);
                if let Some(start) = request.windows(4).position(|window| window == b"\r\n\r\n") {
                    break start + 4;
                }
            };
            let headers = String::from_utf8(request[..header_end].to_vec())
                .expect("request headers are UTF-8");
            let body_length = headers
                .lines()
                .find_map(|line| {
                    line.to_ascii_lowercase()
                        .strip_prefix("content-length:")
                        .and_then(|value| value.trim().parse::<usize>().ok())
                })
                .expect("JSON request has content length");
            while request.len() - header_end < body_length {
                let count = socket.read(&mut buffer).await.expect("body read succeeds");
                assert!(count > 0, "request has complete body");
                request.extend_from_slice(&buffer[..count]);
            }
            let response = format!(
                "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                declared_length.unwrap_or(body.len())
            );
            socket
                .write_all(response.as_bytes())
                .await
                .expect("response write succeeds");
            requests.push(String::from_utf8(request).expect("request is UTF-8"));
            }
            assert!(tokio::time::timeout(Duration::from_millis(100), listener.accept()).await.is_err(), "no automatic retry or extra call");
            requests
            }).await.expect("server finishes within its deadline")
        });
        (client, server)
    }

    #[tokio::test]
    async fn sends_one_json_attempt_with_original_token_and_fixed_identity() {
        let body = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#.to_owned();
        let (client, server) = serve(200, body, None, 1).await;
        let token = "original-token"
            .parse::<Token>()
            .expect("fixed token is valid");
        let challenge_id = Id::new();

        let response = client
            .verify(
                &token,
                challenge_id,
                Purpose::Authentication,
                OPERATION_ID.parse().expect("valid operation"),
                Duration::from_secs(2),
            )
            .await
            .expect("fixed supplier response decodes");
        assert!(matches!(response, Context::Matched(_)));
        let request = tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("test server completes in time")
            .expect("test server completes");
        let (headers, body) = request[0].split_once("\r\n\r\n").expect("HTTP body exists");
        assert!(headers.starts_with("POST /turnstile/v0/siteverify HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("content-type: application/json")
        );
        let json: serde_json::Value = serde_json::from_str(body).expect("request is JSON");
        assert_eq!(json["secret"], "test-secret");
        assert_eq!(json["response"], "original-token");
        assert_eq!(json["idempotency_key"], challenge_id.to_string());
        assert!(json.get("remoteip").is_none());
    }

    #[tokio::test]
    async fn reused_client_checks_each_operations_context() {
        let body = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#.to_owned();
        let (client, server) = serve(200, body, None, 3).await;
        assert_eq!(format!("{client:?}"), r#"Client("[redacted]")"#);
        let cases = [
            ("first-token", Purpose::Authentication, OPERATION_ID, true),
            (
                "second-token",
                Purpose::Authentication,
                "01890f6e-7b80-7000-8000-000000000001",
                false,
            ),
            ("third-token", Purpose::MobileChange, OPERATION_ID, false),
        ];
        let mut challenge_ids = Vec::new();
        for (token, purpose, operation_id, matched) in cases {
            let challenge_id = Id::new();
            challenge_ids.push(challenge_id.to_string());
            let context = client
                .verify(
                    &token.parse().expect("valid token"),
                    challenge_id,
                    purpose,
                    operation_id.parse().expect("valid operation"),
                    Duration::from_secs(1),
                )
                .await
                .expect("valid supplier response");
            if matched {
                assert!(matches!(context, Context::Matched(_)));
            } else {
                assert!(matches!(context, Context::Mismatch));
            }
        }
        let requests = server.await.expect("server verifies the call count");
        for ((request, challenge_id), (token, ..)) in requests.iter().zip(challenge_ids).zip(cases)
        {
            let (_, body) = request.split_once("\r\n\r\n").expect("body exists");
            let body: serde_json::Value = serde_json::from_str(body).expect("JSON body");
            assert_eq!(body["secret"], "test-secret");
            assert_eq!(body["response"], token);
            assert_eq!(body["idempotency_key"], challenge_id);
        }
    }

    #[tokio::test]
    async fn client_uses_its_own_hostname_allowlist() {
        let body = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"other.example","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#.to_owned();
        let (client, server) = serve(200, body, None, 1).await;
        let context = client
            .verify(
                &"original-token".parse().expect("valid token"),
                Id::new(),
                Purpose::Authentication,
                OPERATION_ID.parse().expect("valid operation"),
                Duration::from_secs(1),
            )
            .await
            .expect("valid response");
        assert!(matches!(context, Context::Mismatch));
        server.await.expect("server verifies one call");
    }

    #[tokio::test]
    async fn treats_redirect_server_error_and_invalid_json_as_technical_failures() {
        let token = "original-token"
            .parse::<Token>()
            .expect("fixed token is valid");
        for (status, body) in [
            (302, String::new()),
            (
                503,
                r#"{"success":false,"error-codes":["internal-error"]}"#.to_owned(),
            ),
            (200, "not-json".to_owned()),
        ] {
            let (client, server) = serve(status, body, None, 1).await;
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                client.verify(
                    &token,
                    Id::new(),
                    Purpose::Authentication,
                    OPERATION_ID.parse().expect("valid operation"),
                    Duration::from_secs(1),
                ),
            )
            .await
            .expect("client attempt completes without retry");
            match (status, result) {
                (302, Err(Error::Status { status, body })) => {
                    assert_eq!(status.as_u16(), 302);
                    assert!(body.as_ref().is_empty());
                }
                (503, Err(Error::Status { status, body })) => {
                    assert_eq!(status.as_u16(), 503);
                    assert_eq!(
                        body.as_ref(),
                        r#"{"success":false,"error-codes":["internal-error"]}"#
                    );
                    assert!(!format!("{body:?}").contains("internal-error"));
                }
                (200, Err(Error::Decode(_))) => {}
                _ => panic!("unexpected Siteverify attempt result"),
            }
            tokio::time::timeout(Duration::from_secs(2), server)
                .await
                .expect("test server completes in time")
                .expect("test server completes");
        }
    }

    #[tokio::test]
    async fn distinguishes_rejection_decode_and_body_failures_with_success_status() {
        for (case, body, declared_length) in [
            (
                "rejected",
                r#"{"success":false,"error-codes":["invalid-input-response"]}"#,
                None,
            ),
            ("decode", r#"{"success":"sensitive-supplier-text"}"#, None),
            ("body", "cut", Some(4)),
        ] {
            let (client, server) = serve(200, body.to_owned(), declared_length, 1).await;
            let result = client
                .verify(
                    &"original-token".parse().expect("valid token"),
                    Id::new(),
                    Purpose::Authentication,
                    OPERATION_ID.parse().expect("valid operation"),
                    Duration::from_secs(1),
                )
                .await;
            match (case, result) {
                ("rejected", Ok(Context::SupplierFailed { error_codes })) => {
                    assert_eq!(error_codes, [Code::InvalidInputResponse]);
                }
                ("body", Err(ref error @ Error::Reqwest(ref source))) => {
                    assert!(std::error::Error::source(&error).is_some());
                    assert!(source.url().is_none());
                }
                ("decode", Err(ref error @ Error::Decode(_))) => {
                    assert!(std::error::Error::source(&error).is_some());
                    assert!(!format!("{error:?} {error}").contains("sensitive-supplier-text"));
                }
                _ => panic!("unexpected Siteverify result"),
            }
            server.await.expect("server verifies one call");
        }
    }

    #[tokio::test]
    async fn decode_diagnostics_hide_supplier_values_and_preserve_the_cause() {
        let (client, server) = serve(200, r#""sensitive-supplier-text""#.to_owned(), None, 1).await;
        let response = client
            .inner
            .post(client.address.clone())
            .json(&false)
            .send()
            .await
            .expect("local response arrives");
        let source = response
            .json::<bool>()
            .await
            .expect_err("a string is not a boolean");
        assert!(format!("{source:?}").contains("sensitive-supplier-text"));
        let error = Error::from(source);
        assert!(!format!("{error:?} {error}").contains("sensitive-supplier-text"));
        assert!(std::error::Error::source(&error).is_some());
        server.await.expect("server verifies one call");
    }

    #[tokio::test]
    async fn retains_status_when_error_body_is_truncated() {
        let (client, server) = serve(503, "cut".to_owned(), Some(4), 1).await;
        let token = "original-token"
            .parse::<Token>()
            .expect("fixed token is valid");

        match client
            .verify(
                &token,
                Id::new(),
                Purpose::Authentication,
                OPERATION_ID.parse().expect("valid operation"),
                Duration::from_secs(1),
            )
            .await
        {
            Err(Error::StatusRead { status, source }) => {
                assert_eq!(status.as_u16(), 503);
                assert!(source.url().is_none());
            }
            _ => panic!("truncated error body must retain status and a sanitized cause"),
        }
        tokio::time::timeout(Duration::from_secs(2), server)
            .await
            .expect("test server completes in time")
            .expect("test server completes");
    }

    #[tokio::test]
    async fn does_not_retry_a_lost_response() {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local test listener binds");
        let address = listener
            .local_addr()
            .expect("local listener has an address");
        let address = Url::parse(&format!("http://{address}/turnstile/v0/siteverify"))
            .expect("local test URL parses");
        let client =
            Client::with_address(address, Settings::new(allowed_hostnames()), server_secret())
                .expect("test client builds");
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.expect("first request arrives");
            let mut buffer = [0_u8; 4096];
            assert!(
                socket
                    .read(&mut buffer)
                    .await
                    .expect("request read succeeds")
                    > 0
            );
            drop(socket);
            tokio::time::timeout(Duration::from_secs(2), listener.accept())
                .await
                .is_ok()
        });
        let token = "original-token"
            .parse::<Token>()
            .expect("fixed token is valid");

        match client
            .verify(
                &token,
                Id::new(),
                Purpose::Authentication,
                OPERATION_ID.parse().expect("valid operation"),
                Duration::from_secs(1),
            )
            .await
        {
            Err(Error::Reqwest(source)) => assert!(source.url().is_none()),
            _ => panic!("lost response must retain a sanitized transport cause"),
        }
        assert!(
            !tokio::time::timeout(Duration::from_secs(3), server)
                .await
                .expect("test server completes in time")
                .expect("test server completes"),
            "lost response must not trigger a second connection"
        );
    }

    #[tokio::test]
    async fn bounds_success_and_error_bodies_with_and_without_declared_lengths() {
        for status in [200, 503] {
            for framing in ["length", "chunked", "close"] {
                for size in [super::RESPONSE_LIMIT, super::RESPONSE_LIMIT + 1] {
                    let listener = TcpListener::bind("127.0.0.1:0").await.expect("loopback");
                    let address = listener.local_addr().expect("address");
                    let client = Client::with_address(
                        Url::parse(&format!("http://{address}/siteverify")).expect("URL"),
                        Settings::new(allowed_hostnames()),
                        server_secret(),
                    )
                    .expect("client");
                    let server = tokio::spawn(async move {
                        let (mut socket, _) = listener.accept().await.expect("request");
                        let mut request = [0; 4096];
                        assert!(socket.read(&mut request).await.expect("read request") > 0);
                        let mut body =
                            r#"{"success":false,"error-codes":["invalid-input-response"]}"#
                                .to_owned();
                        body.extend(std::iter::repeat_n(' ', size - body.len()));
                        let response = if framing == "chunked" {
                            format!(
                                "HTTP/1.1 {status} Test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{:x}\r\n{}\r\n1\r\n{}\r\n0\r\n\r\n",
                                size - 1,
                                &body[..size - 1],
                                &body[size - 1..]
                            )
                        } else if framing == "length" {
                            format!(
                                "HTTP/1.1 {status} Test\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n{body}"
                            )
                        } else {
                            format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n\r\n{body}")
                        };
                        socket
                            .write_all(response.as_bytes())
                            .await
                            .expect("response");
                    });
                    let result = client
                        .verify(
                            &"fictional-token".parse().expect("token"),
                            Id::new(),
                            Purpose::Authentication,
                            OPERATION_ID.parse().expect("operation"),
                            Duration::from_secs(2),
                        )
                        .await;
                    if size > super::RESPONSE_LIMIT {
                        assert!(matches!(result, Err(Error::TooLarge)));
                    } else if status == 200 {
                        assert!(matches!(result, Ok(Context::SupplierFailed { .. })));
                    } else {
                        assert!(matches!(result, Err(Error::Status { .. })));
                    }
                    server.await.expect("server ends");
                }
            }
        }
    }

    #[tokio::test]
    async fn exhausted_budget_prevents_an_http_attempt() {
        let client = Client::new(Settings::new(allowed_hostnames()), server_secret())
            .expect("fixed client builds");
        let token = "original-token"
            .parse::<Token>()
            .expect("fixed token is valid");
        assert!(matches!(
            client
                .verify(
                    &token,
                    Id::new(),
                    Purpose::Authentication,
                    OPERATION_ID.parse().expect("valid operation"),
                    Duration::ZERO
                )
                .await,
            Err(Error::Expired)
        ));
    }
}
