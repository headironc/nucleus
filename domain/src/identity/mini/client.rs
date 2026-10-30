//! Single-attempt WeChat phone exchange and process-local access-token cache.

use std::{fmt, time::Duration};

use reqwest::{Response, redirect::Policy};
use serde::{Deserialize, Serialize};
use tokio::{
    sync::Mutex,
    time::{Instant, timeout_at},
};
use url::Url;
use zeroize::Zeroizing;

use crate::identity::{
    mini::{AppId, Code},
    mobile::Mobile,
};

const LIMIT: usize = 64 * 1024;

pub struct Secret(Zeroizing<String>);

impl Secret {
    pub fn new(value: Zeroizing<String>) -> Result<Self, SecretError> {
        if value.is_empty()
            || value
                .chars()
                .any(|value| value.is_whitespace() || value.is_control())
        {
            return Err(SecretError);
        }
        Ok(Self(value))
    }
}

#[derive(Debug, thiserror::Error)]
#[error("mini-program secret must be nonempty without whitespace or controls")]
pub struct SecretError;

struct Token {
    value: Zeroizing<String>,
    expires: Instant,
}

pub struct Client {
    http: reqwest::Client,
    base: Url,
    app_id: AppId,
    secret: Secret,
    token: Mutex<Option<Token>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MiniClient([redacted])")
    }
}

impl Client {
    pub fn new(app_id: AppId, secret: Secret) -> Result<Self, BuildError> {
        Self::build(Url::parse("https://api.weixin.qq.com/")?, app_id, secret)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub fn for_test(port: u16, app_id: AppId, secret: Secret) -> Result<Self, BuildError> {
        Self::build(
            Url::parse(&format!("http://127.0.0.1:{port}/"))?,
            app_id,
            secret,
        )
    }

    fn build(base: Url, app_id: AppId, secret: Secret) -> Result<Self, BuildError> {
        let _ = rustls::crypto::ring::default_provider().install_default();
        let http = reqwest::Client::builder()
            .tls_backend_rustls()
            .redirect(Policy::none())
            .retry(reqwest::retry::never())
            .no_proxy()
            .connect_timeout(Duration::from_secs(3))
            .build()?;
        Ok(Self {
            http,
            base,
            app_id,
            secret,
            token: Mutex::new(None),
        })
    }

    pub fn app_id(&self) -> &AppId {
        &self.app_id
    }

    pub(crate) async fn exchange(
        &self,
        code: &Code,
        remaining: Duration,
    ) -> Result<Outcome, Error> {
        let started = Instant::now();
        let result = timeout_at(
            started + remaining.min(Duration::from_secs(10)),
            self.perform(code),
        )
        .await;
        match result {
            Ok(result) => result,
            Err(_) => Err(Error::Budget),
        }
    }

    async fn perform(&self, code: &Code) -> Result<Outcome, Error> {
        let token = self.access_token().await?;
        let mut address = self.base.join("wxa/business/getuserphonenumber")?;
        address
            .query_pairs_mut()
            .append_pair("access_token", &token);
        #[derive(Serialize)]
        struct Input<'a> {
            code: &'a str,
        }
        let response = self
            .http
            .post(address)
            .json(&Input { code: &code.0 })
            .send()
            .await?;
        let body = body(response).await?;
        let response: PhoneResponse = serde_json::from_slice(&body).map_err(decode)?;
        match response.errcode {
            0 => response
                .phone_info
                .ok_or(Error::Structure)?
                .check(&self.app_id),
            40029 | 40163 => Ok(Outcome::Invalid),
            errcode => Err(Error::Supplier(errcode)),
        }
    }

    async fn access_token(&self) -> Result<Zeroizing<String>, Error> {
        let mut cache = self.token.lock().await;
        if let Some(token) = cache
            .as_ref()
            .filter(|token| Instant::now() < token.expires)
        {
            return Ok(token.value.clone());
        }
        #[derive(Serialize)]
        struct Input<'a> {
            grant_type: &'static str,
            appid: &'a str,
            secret: &'a str,
            force_refresh: bool,
        }
        let address = self.base.join("cgi-bin/stable_token")?;
        let issued = Instant::now();
        let body = body(
            self.http
                .post(address)
                .json(&Input {
                    grant_type: "client_credential",
                    appid: self.app_id.as_ref(),
                    secret: &self.secret.0,
                    force_refresh: false,
                })
                .send()
                .await?,
        )
        .await?;
        let response: TokenResponse = serde_json::from_slice(&body).map_err(decode)?;
        match response.errcode {
            None | Some(0) => {
                let access_token = response.access_token.ok_or(Error::Structure)?;
                let expires_in = response.expires_in.ok_or(Error::Structure)?;
                if access_token.is_empty()
                    || access_token
                        .chars()
                        .any(|value| value.is_whitespace() || value.is_control())
                    || expires_in <= 60
                {
                    return Err(Error::Structure);
                }
                let expires = issued
                    .checked_add(Duration::from_secs(expires_in - 60))
                    .ok_or(Error::Structure)?;
                if Instant::now() >= expires {
                    return Err(Error::Budget);
                }
                *cache = Some(Token {
                    value: access_token.clone(),
                    expires,
                });
                Ok(access_token)
            }
            Some(errcode) => Err(Error::Supplier(errcode)),
        }
    }
}

async fn body(mut response: Response) -> Result<Zeroizing<Vec<u8>>, Error> {
    if !response.status().is_success() {
        return Err(Error::Status(response.status().as_u16()));
    }
    let mut body = Zeroizing::new(Vec::new());
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > LIMIT - body.len() {
            return Err(Error::TooLarge);
        }
        body.extend_from_slice(&chunk);
    }
    Ok(body)
}

pub(crate) enum Outcome {
    Mobile(Mobile),
    Invalid,
    Unsupported,
}

#[derive(Deserialize)]
struct TokenResponse {
    errcode: Option<i64>,
    access_token: Option<Zeroizing<String>>,
    expires_in: Option<u64>,
}

#[derive(Deserialize)]
struct PhoneResponse {
    errcode: i64,
    phone_info: Option<Phone>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Phone {
    phone_number: Zeroizing<String>,
    pure_phone_number: Zeroizing<String>,
    country_code: String,
    watermark: Watermark,
}

#[derive(Deserialize)]
struct Watermark {
    appid: String,
}

impl Phone {
    fn check(self, app_id: &AppId) -> Result<Outcome, Error> {
        if self.watermark.appid != app_id.as_ref()
            || self.country_code.is_empty()
            || !self.country_code.bytes().all(|byte| byte.is_ascii_digit())
            || self.pure_phone_number.is_empty()
            || !self
                .pure_phone_number
                .bytes()
                .all(|byte| byte.is_ascii_digit())
        {
            return Err(Error::Structure);
        }
        let full = Zeroizing::new(format!(
            "{}{}",
            self.country_code,
            self.pure_phone_number.as_str()
        ));
        let supplied = self
            .phone_number
            .strip_prefix('+')
            .unwrap_or(&self.phone_number);
        if supplied != full.as_str()
            && !(self.country_code == "86" && supplied == self.pure_phone_number.as_str())
        {
            return Err(Error::Structure);
        }
        if self.country_code != "86" {
            return Ok(Outcome::Unsupported);
        }
        match self.pure_phone_number.parse() {
            Ok(mobile) => Ok(Outcome::Mobile(mobile)),
            Err(_) => Ok(Outcome::Unsupported),
        }
    }
}

fn decode(_: serde_json::Error) -> Error {
    Error::Structure
}

#[derive(Debug, thiserror::Error)]
pub enum BuildError {
    #[error("invalid mini-program supplier address")]
    Address(#[from] url::ParseError),
    #[error("mini-program HTTP client initialization failed")]
    Http(#[from] reqwest::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("mini-program exchange budget exhausted")]
    Budget,
    #[error("mini-program supplier returned HTTP {0}")]
    Status(u16),
    #[error("mini-program supplier returned code {0}")]
    Supplier(i64),
    #[error("mini-program supplier response has invalid structure")]
    Structure,
    #[error("mini-program supplier response exceeds 64 KiB")]
    TooLarge,
    #[error("mini-program transport failed")]
    Transport(#[source] reqwest::Error),
    #[error("mini-program supplier address is invalid")]
    Address(#[from] url::ParseError),
}

impl From<reqwest::Error> for Error {
    fn from(value: reqwest::Error) -> Self {
        Self::Transport(value.without_url())
    }
}

impl Error {
    pub(crate) fn code(&self) -> Option<i64> {
        match self {
            Self::Supplier(code) => Some(*code),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{sync::Arc, time::Duration};

    use tokio::time::{Instant, timeout};

    use crate::identity::{
        authentication::Supplier,
        mini::client::{Error, Outcome},
    };

    #[tokio::test]
    async fn token_ttl_has_the_safety_margin_and_phone_errors_do_not_refresh_it() {
        let mut supplier = Supplier::new().await;
        let client = Arc::new(supplier.mini_client());
        let before = Instant::now();
        let worker = Arc::clone(&client);
        let task = tokio::spawn(async move {
            worker
                .exchange(&"first-code".parse().expect("code"), Duration::from_secs(5))
                .await
        });
        supplier.request().await.reply(
            200,
            r#"{"access_token":"private-access-token","expires_in":61}"#.into(),
        );
        supplier
            .request()
            .await
            .reply(200, r#"{"errcode":40029}"#.into());
        assert!(matches!(task.await.expect("worker"), Ok(Outcome::Invalid)));
        let cache = client.token.lock().await;
        let token = cache.as_ref().expect("cached token");
        assert!(token.expires > before && token.expires <= Instant::now() + Duration::from_secs(1));
        drop(cache);
        let worker = Arc::clone(&client);
        let task = tokio::spawn(async move {
            worker
                .exchange(
                    &"second-code".parse().expect("code"),
                    Duration::from_secs(5),
                )
                .await
        });
        let request = supplier.request().await;
        assert_eq!(
            request.body()["code"],
            "second-code",
            "cached token is reused without force refresh"
        );
        request.reply(
            200,
            r#"{"errcode":40001,"errmsg":"private-phone-code"}"#.into(),
        );
        let error = task.await.expect("worker").err().expect("supplier failure");
        assert_eq!(error.code(), Some(40001));
        assert!(!format!("{error:?}").contains("private"));
        assert!(
            timeout(Duration::from_millis(20), supplier.observe())
                .await
                .is_err(),
            "no retry or automatic token refresh"
        );
        supplier.stop().await;
    }

    #[tokio::test]
    async fn oversized_and_non_success_responses_are_bounded_and_never_retried() {
        for (status, body, oversized) in [
            (200, "x".repeat(65_537), true),
            (302, "private-redirect".into(), false),
            (503, "private-server-body".into(), false),
        ] {
            let mut supplier = Supplier::new().await;
            let client = supplier.mini_client();
            let task = tokio::spawn(async move {
                client
                    .exchange(
                        &"private-code".parse().expect("code"),
                        Duration::from_secs(5),
                    )
                    .await
            });
            supplier.request().await.reply(status, body);
            let error = task.await.expect("worker").err().expect("refused response");
            if oversized {
                assert!(matches!(error, Error::TooLarge));
            } else {
                assert!(matches!(error, Error::Status(actual) if actual == status));
            }
            assert!(!format!("{error:?}").contains("private"));
            assert!(
                timeout(Duration::from_millis(20), supplier.observe())
                    .await
                    .is_err()
            );
            supplier.stop().await;
        }
    }

    #[tokio::test]
    async fn token_preparation_and_phone_exchange_share_one_total_budget() {
        let mut supplier = Supplier::new().await;
        let client = supplier.mini_client();
        let task = tokio::spawn(async move {
            client
                .exchange(
                    &"private-code".parse().expect("code"),
                    Duration::from_millis(100),
                )
                .await
        });
        let token = supplier.request().await;
        tokio::time::sleep(Duration::from_millis(60)).await;
        token.reply(
            200,
            r#"{"access_token":"private-token","expires_in":7200}"#.into(),
        );
        let phone = supplier.request().await;
        assert!(matches!(
            timeout(Duration::from_millis(80), task)
                .await
                .expect("remaining budget is shared")
                .expect("worker"),
            Err(Error::Budget)
        ));
        drop(phone);
        supplier.stop().await;
    }
}
