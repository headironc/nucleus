//! Loopback supplier stubs for Siteverify and PNVS.

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use axum::{Router, body::Bytes, http::header, routing::post};
use domain::identity::challenge::{self, client};
use nonempty::NonEmpty;
use tokio::{net::TcpListener, task::JoinHandle};

/// Loopback Siteverify stub that counts every verification call.
pub(crate) struct Supplier {
    port: u16,
    pub(crate) task: JoinHandle<()>,
    pub(crate) calls: Arc<AtomicUsize>,
}

impl Supplier {
    pub(crate) async fn echoing() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback captcha");
        let port = listener.local_addr().expect("address").port();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let router=Router::new().route("/siteverify",post(move |body:Bytes| {
            let observed=Arc::clone(&observed);
            async move {
                observed.fetch_add(1,Ordering::SeqCst);
                let input:serde_json::Value=serde_json::from_slice(&body).expect("captcha JSON");
                let token=input["response"].as_str().expect("fixture operation token");
                let (action, operation) = if let Some(operation) = token.strip_prefix("mobile:") {
                    ("send_mobile_change_code", operation)
                } else if let Some(operation) = token.strip_prefix("binding:") {
                    ("send_mobile_binding_code", operation)
                } else {
                    ("send_authentication_code", token)
                };
                axum::Json(serde_json::json!({"success":true,"challenge_ts":chrono::Utc::now(),"hostname":"example.com","action":action,"cdata":operation}))
            }
        }));
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.expect("captcha server");
        });
        Self { port, task, calls }
    }

    pub(crate) async fn new() -> Self {
        Self::responding(r#"{"success":false,"error-codes":["invalid-input-response"]}"#.into())
            .await
    }

    pub(crate) async fn responding(body: String) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("local supplier");
        let port = listener.local_addr().expect("supplier address").port();
        let calls = Arc::new(AtomicUsize::new(0));
        let observed = Arc::clone(&calls);
        let body = Bytes::from(body);
        let router = Router::new().route(
            "/siteverify",
            post(move |_: Bytes| {
                let observed = Arc::clone(&observed);
                let body = body.clone();
                async move {
                    observed.fetch_add(1, Ordering::SeqCst);
                    (
                        [
                            (header::CONTENT_TYPE, "application/json"),
                            (header::CONNECTION, "close"),
                        ],
                        body,
                    )
                }
            }),
        );
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("fixture supplier");
        });
        Self { port, task, calls }
    }

    pub(crate) fn client(&self) -> client::Client {
        client::Client::for_test(
            self.port,
            challenge::settings::Settings::new(
                challenge::settings::AllowedHostnames::new(NonEmpty::new("example.com".into()))
                    .expect("hostnames"),
            ),
            "fictional-secret".parse().expect("secret"),
        )
        .expect("fixture client")
    }
}

impl Drop for Supplier {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) struct Mini {
    port: u16,
    task: JoinHandle<()>,
    pub(crate) calls: Arc<AtomicUsize>,
    pub(crate) tokens: Arc<AtomicUsize>,
}

impl Mini {
    pub(crate) async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0")
            .await
            .expect("mini supplier");
        let port = listener.local_addr().expect("mini address").port();
        let calls = Arc::new(AtomicUsize::new(0));
        let tokens = Arc::new(AtomicUsize::new(0));
        let token_calls = Arc::clone(&tokens);
        let phone_calls = Arc::clone(&calls);
        let router = Router::new()
            .route("/cgi-bin/stable_token", post(move |axum::Json(input): axum::Json<serde_json::Value>| {
                token_calls.fetch_add(1, Ordering::SeqCst);
                assert_eq!(input["appid"], "mini-fixture"); assert_eq!(input["force_refresh"], false);
                async { axum::Json(serde_json::json!({"access_token":"fictional-access-token","expires_in":7200})) }
            }))
            .route("/wxa/business/getuserphonenumber", post(move |axum::Json(input): axum::Json<serde_json::Value>| {
                phone_calls.fetch_add(1, Ordering::SeqCst);
                async move {
                    match input["code"].as_str().expect("phone code") {
                        "invalid" => axum::Json(serde_json::json!({"errcode":40029})),
                        "unsupported" => axum::Json(serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"12025550100","purePhoneNumber":"2025550100","countryCode":"1","watermark":{"appid":"mini-fixture"}}})),
                        _ => axum::Json(serde_json::json!({"errcode":0,"phone_info":{"phoneNumber":"8613800000000","purePhoneNumber":"13800000000","countryCode":"86","watermark":{"appid":"mini-fixture"}}})),
                    }
                }
            }));
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .await
                .expect("mini supplier server");
        });
        Self {
            port,
            task,
            calls,
            tokens,
        }
    }

    pub(crate) fn client(&self) -> domain::identity::mini::client::Client {
        domain::identity::mini::client::Client::for_test(
            self.port,
            "mini-fixture".parse().expect("mini identity"),
            domain::identity::mini::client::Secret::new(zeroize::Zeroizing::new(
                "fictional-mini-secret".into(),
            ))
            .expect("secret"),
        )
        .expect("mini client")
    }
}

impl Drop for Mini {
    fn drop(&mut self) {
        self.task.abort();
    }
}

pub(crate) async fn message() -> domain::identity::message::client::Client {
    let supplier = fixture::supplier::Supplier::start().await;
    message_at(supplier.address(), supplier.authority())
}

pub(crate) fn message_at(
    address: std::net::SocketAddr,
    authority: &[u8],
) -> domain::identity::message::client::Client {
    message_with_timeout(address, authority, 1000)
}

pub(crate) fn message_with_timeout(
    address: std::net::SocketAddr,
    authority: &[u8],
    timeout_milliseconds: u64,
) -> domain::identity::message::client::Client {
    use domain::identity::message::{
        client::Client,
        credentials::{AccessKey, Id, Secret},
        settings::{Settings, Signature, TemplateCode, Templates},
    };
    Client::for_test(
        address,
        reqwest::Certificate::from_der(authority).expect("fixture certificate"),
        Settings::new(
            Signature::new("测试".into()).expect("signature"),
            Templates::new(
                TemplateCode::new("auth".into()).expect("template"),
                TemplateCode::new("change".into()).expect("template"),
                TemplateCode::new("bind".into()).expect("template"),
            ),
            timeout_milliseconds,
        )
        .expect("settings"),
        AccessKey::new(
            Id::new("FictionalAccessKey".into()).expect("id"),
            Secret::new("fictional-secret".into()).expect("secret"),
        ),
    )
    .expect("unused loopback client")
}

pub(crate) fn accept_sms(request: fixture::supplier::Request) {
    let text = std::str::from_utf8(&request.bytes).expect("fictional request bytes");
    let target = text
        .lines()
        .next()
        .expect("request line")
        .split(' ')
        .nth(1)
        .expect("request target");
    let url =
        url::Url::parse(&format!("https://dypnsapi.aliyuncs.com{target}")).expect("supplier URL");
    let message_id = url
        .query_pairs()
        .find(|(name, _)| name == "OutId")
        .expect("original OutId")
        .1
        .into_owned();
    let body = serde_json::json!({"Code":"OK","Success":true,"Model":{"OutId":message_id,"BizId":"fictional-biz","RequestId":"57fdfbd7-e79d-465a-a121-eab3e91e7290"}}).to_string();
    request
        .reply
        .send(
            format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            )
            .into_bytes(),
        )
        .expect("supplier acceptance");
}
