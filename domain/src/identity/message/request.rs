//! Construction and single-attempt execution of a signed PNVS request.

use std::{
    fmt::{self, Debug, Formatter},
    time::{Duration, Instant},
};

use chrono::{DateTime, Datelike, SecondsFormat, Utc};
use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, utf8_percent_encode};
use reqwest::{
    Method, StatusCode,
    header::{ACCEPT, AUTHORIZATION, HOST, HeaderMap, HeaderValue},
};
use ring::hmac::{self, Key};
use serde::Serialize;
use sha2::{Digest, Sha256};
use thiserror::Error;
use url::Url;
use uuid::Uuid;
use zeroize::Zeroizing;

use crate::identity::{
    challenge::Purpose,
    message::{
        Code, Id,
        access_key::{AccessKey, Secret},
        response::{Failure, Outcome, Response, Unknown},
        settings::Settings,
    },
    mobile::Mobile,
};

const ADDRESS: &str = "https://dypnsapi.aliyuncs.com/";
const HOSTNAME: &str = "dypnsapi.aliyuncs.com";
const ALGORITHM: &str = "ACS3-HMAC-SHA256";
const RESPONSE_LIMIT: usize = 64 * 1024;
const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const QUERY_ESCAPE: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'~');

/// Original message values and the remaining whole seconds at construction.
pub struct Input<'a> {
    pub mobile: &'a Mobile,
    pub code: &'a Code,
    pub message_id: Id,
    pub valid_seconds: u64,
    pub purpose: Purpose,
}

/// Fresh per-call signing context supplied by the caller.
pub struct Context {
    pub date: DateTime<Utc>,
    pub nonce: Uuid,
}

/// An immutable signed request. Debug output hides all request material.
pub struct Request<'a> {
    client: &'a reqwest::Client,
    inner: reqwest::Request,
    message_id: Id,
}

impl<'a> Request<'a> {
    /// Construct a request without I/O. The caller owns sending eligibility.
    pub(super) fn new(
        client: &'a reqwest::Client,
        access_key: &AccessKey,
        settings: &Settings,
        input: Input<'_>,
        context: Context,
    ) -> Result<Self, BuildError> {
        if input.valid_seconds < 60 || input.valid_seconds > i64::MAX.unsigned_abs() {
            return Err(BuildError::Validity);
        }
        if !(1..=9999).contains(&context.date.year()) {
            return Err(BuildError::Date);
        }
        let minutes = input.valid_seconds.div_ceil(60).to_string();
        let template_param = Zeroizing::new(serde_json::to_string(&TemplateParam {
            code: input.code.as_ref(),
            min: &minutes,
        })?);
        let seconds = input.valid_seconds.to_string();
        let message_id = input.message_id.to_string();
        let query = query([
            ("CountryCode", "86"),
            ("PhoneNumber", input.mobile.as_ref()),
            ("SignName", settings.signature()),
            ("TemplateCode", settings.code(input.purpose)),
            ("TemplateParam", template_param.as_str()),
            ("OutId", message_id.as_str()),
            ("ValidTime", seconds.as_str()),
            ("Interval", "60"),
            ("AutoRetry", "0"),
            ("ReturnVerifyCode", "false"),
            ("CodeLength", "8"),
            ("CodeType", "1"),
            ("DuplicatePolicy", "1"),
        ]);

        // Parse the already-encoded query once, before signing its final bytes.
        let address = Zeroizing::new(format!("{ADDRESS}?{}", query.as_str()));
        let url = match Url::parse(&address) {
            Ok(url) if url.query() == Some(query.as_str()) => url,
            _ => return Err(BuildError::Uri),
        };
        let date = context.date.to_rfc3339_opts(SecondsFormat::Secs, true);
        let nonce = context.nonce.to_string();
        let mut headers = headers(HOSTNAME, "SendSmsVerifyCode", "2017-05-25", &date, &nonce)?;
        let (canonical, signed) = canonical_request(&query, &headers)?;
        let string_to_sign = string_to_sign(&canonical);
        let signature = signature(access_key.secret(), &string_to_sign);
        let authorization = Zeroizing::new(format!(
            "{ALGORITHM} Credential={},SignedHeaders={signed},Signature={signature}",
            access_key.id().as_ref(),
        ));
        let mut authorization = header_value(&authorization)?;
        authorization.set_sensitive(true);
        headers.insert(AUTHORIZATION, authorization);

        let mut request = reqwest::Request::new(Method::POST, url);
        *request.headers_mut() = headers;
        *request.body_mut() = Some(Vec::<u8>::new().into());
        Ok(Self {
            client,
            inner: request,
            message_id: input.message_id,
        })
    }

    /// Execute once, consuming the signed request without changing its bytes.
    /// The caller must first establish sending eligibility and check expiry.
    /// Errors and unknown results never authorize resending the message.
    pub async fn execute(self) -> Result<Attempt, Error> {
        let started = Instant::now();
        let mut response = self.client.execute(self.inner).await?;
        let status = response.status();
        let outcome = if status.is_success() || status.is_client_error() {
            let mut body = Zeroizing::new(Vec::new());
            loop {
                let option = match response.chunk().await {
                    Ok(option) => option,
                    Err(source) => {
                        return Err(Error::Read {
                            status,
                            source: source.without_url(),
                        });
                    }
                };
                let Some(chunk) = option else { break };
                if chunk.len() > RESPONSE_LIMIT - body.len() {
                    return Err(Error::TooLarge { status });
                }
                body.extend_from_slice(&chunk);
            }
            let outcome = if status.is_success() {
                serde_json::from_slice::<Response>(&body)
                    .map(|response| response.check(self.message_id, status))
            } else {
                serde_json::from_slice::<Failure>(&body).map(|response| response.check(status))
            };
            match outcome {
                Ok(outcome) => outcome,
                Err(source) => {
                    return Err(Error::Decode {
                        status,
                        category: source.classify(),
                        line: source.line(),
                        column: source.column(),
                    });
                }
            }
        } else {
            Outcome::Unknown(Unknown::HttpStatus)
        };
        Ok(Attempt {
            outcome,
            status,
            elapsed: started.elapsed(),
        })
    }
}

impl Debug for Request<'_> {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("Request")
            .field(&"[redacted]")
            .finish()
    }
}

/// The result and safe diagnostics after receiving HTTP response headers.
///
/// ```compile_fail
/// use domain::identity::message::{request::Attempt, response::{Outcome, Unknown}};
/// let attempt = Attempt {
///     outcome: Outcome::Unknown(Unknown::HttpStatus),
///     status: Default::default(),
///     elapsed: Default::default(),
/// };
/// ```
#[derive(Debug)]
pub struct Attempt {
    outcome: Outcome,
    status: StatusCode,
    elapsed: Duration,
}

impl Attempt {
    pub fn outcome(&self) -> &Outcome {
        &self.outcome
    }

    pub(crate) fn into_outcome(self) -> Outcome {
        self.outcome
    }

    pub fn status(&self) -> StatusCode {
        self.status
    }

    pub fn elapsed(&self) -> Duration {
        self.elapsed
    }
}

/// A request or response failed; the message outcome remains unknown.
#[derive(Error)]
pub enum Error {
    #[error("PNVS request failed")]
    Reqwest(#[source] reqwest::Error),
    #[error("PNVS response exceeds its byte limit ({status})")]
    TooLarge { status: StatusCode },
    #[error("PNVS response read failed ({status})")]
    Read {
        status: StatusCode,
        #[source]
        source: reqwest::Error,
    },
    #[error("PNVS response decoding failed ({status})")]
    Decode {
        status: StatusCode,
        category: serde_json::error::Category,
        line: usize,
        column: usize,
    },
}

impl Debug for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Reqwest(source) if source.is_decode() => formatter
                .debug_tuple("Reqwest")
                .field(&"[redacted]")
                .finish(),
            Self::Reqwest(source) => formatter.debug_tuple("Reqwest").field(source).finish(),
            Self::Read { status, .. } => formatter
                .debug_struct("Read")
                .field("status", status)
                .finish_non_exhaustive(),
            Self::Decode {
                status,
                category,
                line,
                column,
            } => formatter
                .debug_struct("Decode")
                .field("status", status)
                .field("category", category)
                .field("line", line)
                .field("column", column)
                .finish(),
            Self::TooLarge { status } => formatter
                .debug_struct("TooLarge")
                .field("status", status)
                .finish(),
        }
    }
}

impl From<reqwest::Error> for Error {
    fn from(error: reqwest::Error) -> Self {
        Self::Reqwest(error.without_url())
    }
}

/// Local request construction failed; no external call has occurred.
#[derive(Debug, Error)]
pub enum BuildError {
    #[error("PNVS remaining validity must be at least 60 seconds and fit int64")]
    Validity,
    #[error("PNVS context date requires a four-digit positive year")]
    Date,
    #[error("PNVS URI could not preserve the encoded query")]
    Uri,
    #[error("invalid PNVS request header")]
    Header,
    #[error("PNVS template serialization failed")]
    Template(#[from] serde_json::Error),
}

#[derive(Serialize)]
struct TemplateParam<'a> {
    code: &'a str,
    min: &'a str,
}

fn query<const N: usize>(mut parameters: [(&str, &str); N]) -> Zeroizing<String> {
    parameters.sort_unstable_by_key(|(name, _)| *name);
    let mut query = Zeroizing::new(String::new());
    for (name, value) in parameters {
        if !query.is_empty() {
            query.push('&');
        }
        query.extend(utf8_percent_encode(name, QUERY_ESCAPE));
        query.push('=');
        query.extend(utf8_percent_encode(value, QUERY_ESCAPE));
    }
    query
}

fn headers(
    host: &str,
    action: &str,
    version: &str,
    date: &str,
    nonce: &str,
) -> Result<HeaderMap, BuildError> {
    let mut headers = HeaderMap::new();
    headers.insert(HOST, header_value(host)?);
    headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
    headers.insert("x-acs-action", header_value(action)?);
    headers.insert("x-acs-version", header_value(version)?);
    headers.insert("x-acs-date", header_value(date)?);
    headers.insert("x-acs-signature-nonce", header_value(nonce)?);
    headers.insert("x-acs-content-sha256", HeaderValue::from_static(EMPTY_HASH));
    Ok(headers)
}

fn header_value(value: &str) -> Result<HeaderValue, BuildError> {
    match HeaderValue::from_str(value) {
        Ok(value) => Ok(value),
        Err(_) => Err(BuildError::Header),
    }
}

fn canonical_request(
    query: &str,
    headers: &HeaderMap,
) -> Result<(Zeroizing<String>, String), BuildError> {
    let mut selected: Vec<_> = headers
        .iter()
        .filter(|(name, _)| {
            name.as_str() == "host"
                || name.as_str() == "content-type"
                || name.as_str().starts_with("x-acs-")
        })
        .collect();
    selected.sort_unstable_by_key(|(name, _)| name.as_str());
    let mut canonical_headers = String::new();
    let mut signed = String::new();
    for (name, value) in selected {
        let value = match value.to_str() {
            Ok(value) => value.trim(),
            Err(_) => return Err(BuildError::Header),
        };
        canonical_headers.push_str(name.as_str());
        canonical_headers.push(':');
        canonical_headers.push_str(value);
        canonical_headers.push('\n');
        if !signed.is_empty() {
            signed.push(';');
        }
        signed.push_str(name.as_str());
    }
    Ok((
        Zeroizing::new(format!(
            "POST\n/\n{query}\n{canonical_headers}\n{signed}\n{EMPTY_HASH}"
        )),
        signed,
    ))
}

fn string_to_sign(canonical: &str) -> String {
    format!(
        "{ALGORITHM}\n{}",
        hex::encode(Sha256::digest(canonical.as_bytes()))
    )
}

fn signature(secret: &Secret, string_to_sign: &str) -> String {
    let key = Key::new(hmac::HMAC_SHA256, secret.as_ref().as_bytes());
    hex::encode(hmac::sign(&key, string_to_sign.as_bytes()).as_ref())
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::collections::HashMap;

    use chrono::{TimeZone, Timelike};
    use reqwest::header::CONTENT_TYPE;

    use crate::identity::message::{
        client::Client,
        request::{self, *},
    };

    const EXPECTED_EMPTY_HASH: &str =
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const SIGNED: &str =
        "host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-version";
    const NONCE: &str = "01890f6e-7b80-7000-8000-000000000001";
    const MESSAGE_ID: &str = "01890f6e-7b80-7000-8000-000000000000";
    // Independently computed with Python json, urllib.parse, hashlib and hmac.
    const SPECIAL_QUERY: &str = "AutoRetry=0&CodeLength=8&CodeType=1&CountryCode=86&DuplicatePolicy=1&Interval=60&OutId=01890f6e-7b80-7000-8000-000000000000&PhoneNumber=13800000000&ReturnVerifyCode=false&SignName=%E7%AD%BE%E5%90%8D%20%22%5C%2B%25%26%3D%3F%23&TemplateCode=SMS_%E6%B5%8B%E8%AF%95%2F1&TemplateParam=%7B%22code%22%3A%2200001234%22%2C%22min%22%3A%222%22%7D&ValidTime=61";
    const SPECIAL_SIGNATURE: &str =
        "0b4c059a592f2552319b64d1802bf0c586df2497ef519066394aa73ad9c50e27";

    fn context() -> Context {
        Context {
            date: Utc
                .with_ymd_and_hms(2023, 10, 26, 10, 22, 32)
                .single()
                .expect("fixed UTC date exists"),
            nonce: NONCE.parse().expect("fixed nonce is a UUID"),
        }
    }

    fn access_key() -> AccessKey {
        use crate::identity::message::access_key;
        AccessKey::new(
            access_key::Id::new("YourAccessKeyId".into()).expect("example id"),
            Secret::new("YourAccessKeySecret".into()).expect("example secret"),
        )
    }

    fn settings() -> Settings {
        use crate::identity::message::settings::{
            DEFAULT_TIMEOUT_MILLISECONDS, Signature, TemplateCode, Templates,
        };
        Settings::new(
            Signature::new(r#"签名 "\+%&=?#"#.into()).expect("valid signature"),
            Templates::new(
                TemplateCode::new("SMS_测试/1".into()).expect("valid template"),
                TemplateCode::new("SMS_change".into()).expect("valid template"),
                TemplateCode::new("SMS_binding".into()).expect("valid template"),
            ),
            DEFAULT_TIMEOUT_MILLISECONDS,
        )
        .expect("special deployment fields are valid")
    }

    fn client() -> Client {
        Client::new(settings(), access_key()).expect("fixed client builds")
    }

    fn request(
        client: &Client,
        valid_seconds: u64,
        context: Context,
    ) -> Result<Request<'_>, BuildError> {
        client.request(
            Input {
                mobile: &"13800000000".parse().expect("fixed mobile is valid"),
                code: &"00001234".parse().expect("fixed code is valid"),
                message_id: MESSAGE_ID.parse().expect("fixed Message ID is valid"),
                valid_seconds,
                purpose: Purpose::Authentication,
            },
            context,
        )
    }

    #[tokio::test]
    async fn bounds_bodies_before_decoding_for_all_http_framings() {
        for status in [200, 400] {
            for framing in ["length", "chunked", "close"] {
                for size in [RESPONSE_LIMIT, RESPONSE_LIMIT + 1] {
                    let mut supplier = fixture::supplier::Supplier::start().await;
                    let client = Client::for_test(
                        supplier.address(),
                        reqwest::Certificate::from_der(supplier.authority()).expect("fixture CA"),
                        settings(),
                        access_key(),
                    )
                    .expect("client");
                    let mut body = br#"{"Code":"biz.FREQUENCY","Success":false}"#.to_vec();
                    body.resize(size, b' ');
                    let response=match framing {
                        "length"=>[format!("HTTP/1.1 {status} Test\r\nContent-Length: {size}\r\nConnection: close\r\n\r\n").into_bytes(),body].concat(),
                        "chunked"=>[format!("HTTP/1.1 {status} Test\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n{size:x}\r\n").into_bytes(),body,b"\r\n0\r\n\r\n".to_vec()].concat(),
                        _=>[format!("HTTP/1.1 {status} Test\r\nConnection: close\r\n\r\n").into_bytes(),body].concat(),
                    };
                    let execute = request(&client, 300, context()).expect("request").execute();
                    let (result, ()) = tokio::time::timeout(Duration::from_secs(5), async {
                        tokio::join!(execute, async {
                            assert!(supplier.request().await.reply.send(response).is_ok());
                        })
                    })
                    .await
                    .expect("bounded request");
                    if size == RESPONSE_LIMIT {
                        assert!(matches!(
                            result.expect("exact limit allowed").outcome(),
                            Outcome::Rejected(_)
                        ));
                    } else {
                        assert!(
                            matches!(result,Err(Error::TooLarge{status: actual}) if actual.as_u16()==status)
                        );
                    }
                    assert!(
                        tokio::time::timeout(Duration::from_millis(20), supplier.request())
                            .await
                            .is_err()
                    );
                }
            }
        }
    }

    #[test]
    fn matches_the_official_acs3_fixed_vector() {
        // https://help.aliyun.com/zh/sdk/product-overview/v3-request-structure-and-signature
        // Use the fixed-vector date/nonce, not the inconsistent later wire illustration.
        let query = query([
            ("RegionId", "cn-shanghai"),
            (
                "ImageId",
                "win2019_1809_x64_dtc_zh-cn_40G_alibase_20230811.vhd",
            ),
        ]);
        assert_eq!(
            query.as_str(),
            "ImageId=win2019_1809_x64_dtc_zh-cn_40G_alibase_20230811.vhd&RegionId=cn-shanghai"
        );
        let headers = headers(
            "ecs.cn-shanghai.aliyuncs.com",
            "RunInstances",
            "2014-05-26",
            "2023-10-26T10:22:32Z",
            "3156853299f313e23d1673dc12e1703d",
        )
        .expect("official headers are valid");
        assert_eq!(headers["x-acs-content-sha256"], EXPECTED_EMPTY_HASH);
        let (canonical, signed) =
            canonical_request(&query, &headers).expect("official canonical request is valid");
        assert_eq!(signed, SIGNED);
        assert_eq!(
            canonical.as_str(),
            concat!(
                "POST\n/\nImageId=win2019_1809_x64_dtc_zh-cn_40G_alibase_20230811.vhd&RegionId=cn-shanghai\n",
                "host:ecs.cn-shanghai.aliyuncs.com\nx-acs-action:RunInstances\n",
                "x-acs-content-sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855\n",
                "x-acs-date:2023-10-26T10:22:32Z\nx-acs-signature-nonce:3156853299f313e23d1673dc12e1703d\nx-acs-version:2014-05-26\n\n",
                "host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-version\n",
                "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
            )
        );
        let string_to_sign = string_to_sign(&canonical);
        assert_eq!(
            string_to_sign,
            "ACS3-HMAC-SHA256\n7ea06492da5221eba5297e897ce16e55f964061054b7695beedaac1145b1e259"
        );
        assert_eq!(
            signature(access_key().secret(), &string_to_sign),
            "06563a9e1b43f5dfe96b81484da74bceab24a1d853912eee15083a6f0f3283c0"
        );
    }

    #[test]
    fn preserves_the_independent_special_character_request_through_http_assembly() {
        let client = client();
        let request = request(&client, 61, context()).expect("special request is valid");
        assert_eq!(request.inner.method(), Method::POST);
        assert_eq!(
            request.inner.url().as_str(),
            format!("{ADDRESS}?{SPECIAL_QUERY}")
        );
        assert_eq!(request.inner.url().query(), Some(SPECIAL_QUERY));
        assert_eq!(request.inner.url().fragment(), None);
        assert_eq!(request.inner.headers().len(), 8);
        assert_eq!(request.inner.headers()[ACCEPT], "application/json");
        assert!(!request.inner.headers().contains_key(CONTENT_TYPE));
        assert_eq!(
            request.inner.headers()["x-acs-content-sha256"],
            EXPECTED_EMPTY_HASH
        );
        let expected_headers = format!(
            "host:dypnsapi.aliyuncs.com\nx-acs-action:SendSmsVerifyCode\nx-acs-content-sha256:{EXPECTED_EMPTY_HASH}\nx-acs-date:2023-10-26T10:22:32Z\nx-acs-signature-nonce:{NONCE}\nx-acs-version:2017-05-25\n"
        );
        let (canonical, signed) = canonical_request(SPECIAL_QUERY, request.inner.headers())
            .expect("special canonical request is valid");
        assert_eq!(
            canonical.as_str(),
            format!(
                "POST\n/\n{SPECIAL_QUERY}\n{expected_headers}\n{SIGNED}\n{EXPECTED_EMPTY_HASH}"
            )
        );
        assert_eq!(signed, SIGNED);
        assert_eq!(
            string_to_sign(&canonical),
            "ACS3-HMAC-SHA256\n5afb2c5c47f5f5ba9801163c24f369a63553d4c14a354364aa17860d707c1f7d"
        );
        assert_eq!(
            request.inner.headers()[AUTHORIZATION],
            format!(
                "ACS3-HMAC-SHA256 Credential=YourAccessKeyId,SignedHeaders={SIGNED},Signature={SPECIAL_SIGNATURE}"
            )
        );
        assert!(request.inner.headers()[AUTHORIZATION].is_sensitive());

        let http = request.inner;
        assert_eq!(http.method(), Method::POST);
        assert_eq!(http.url().query(), Some(SPECIAL_QUERY));
        assert_eq!(
            http.body().expect("body exists").as_bytes(),
            Some(b"".as_slice())
        );
        assert_eq!(
            http.headers()[AUTHORIZATION],
            format!(
                "ACS3-HMAC-SHA256 Credential=YourAccessKeyId,SignedHeaders={SIGNED},Signature={SPECIAL_SIGNATURE}"
            )
        );
    }

    #[test]
    fn encodes_json_escapes_once_as_a_single_query_value() {
        let json = serde_json::to_string(&TemplateParam {
            code: "引号\"反斜线\\换行\n +/%&=~*",
            min: "2",
        })
        .expect("string fields serialize");
        assert_eq!(json, r#"{"code":"引号\"反斜线\\换行\n +/%&=~*","min":"2"}"#);
        let query = query([("TemplateParam", json.as_str())]);
        assert_eq!(
            query.as_str(),
            "TemplateParam=%7B%22code%22%3A%22%E5%BC%95%E5%8F%B7%5C%22%E5%8F%8D%E6%96%9C%E7%BA%BF%5C%5C%E6%8D%A2%E8%A1%8C%5Cn%20%2B%2F%25%26%3D~%2A%22%2C%22min%22%3A%222%22%7D"
        );
        // Exercise the encoder's key and empty-value rules independently of fixed API keys.
        assert_eq!(
            request::query([("z", ""), ("A +", "AZaz09-._~!*'()")]).as_str(),
            "A%20%2B=AZaz09-._~%21%2A%27%28%29&z="
        );
    }

    #[test]
    fn normalizes_header_names_order_and_outer_whitespace() {
        let mut headers = headers(
            "  dypnsapi.aliyuncs.com  ",
            "SendSmsVerifyCode",
            "2017-05-25",
            "2023-10-26T10:22:32Z",
            NONCE,
        )
        .expect("padded headers are valid");
        headers.insert("X-Acs-Test", HeaderValue::from_static("  one  two  "));
        headers.insert(
            CONTENT_TYPE,
            HeaderValue::from_static("  application/json  "),
        );
        headers.insert(AUTHORIZATION, HeaderValue::from_static("ignored"));
        let (canonical, signed) =
            canonical_request("", &headers).expect("headers can be normalized");
        assert_eq!(
            signed,
            "content-type;host;x-acs-action;x-acs-content-sha256;x-acs-date;x-acs-signature-nonce;x-acs-test;x-acs-version"
        );
        assert!(
            canonical.starts_with(
                "POST\n/\n\ncontent-type:application/json\nhost:dypnsapi.aliyuncs.com\n"
            )
        );
        assert!(canonical.contains("\nx-acs-test:one  two\nx-acs-version:2017-05-25\n\n"));
        assert!(!canonical.contains("ignored"));
        assert!(!canonical.contains("accept:"));
    }

    #[test]
    fn preserves_seconds_and_rounds_only_template_minutes_up() {
        let client = client();
        for (seconds, minutes) in [(60, "1"), (61, "2"), (119, "2"), (120, "2"), (121, "3")] {
            let request = request(&client, seconds, context()).expect("valid remaining seconds");
            let pairs: HashMap<_, _> = request.inner.url().query_pairs().collect();
            assert_eq!(pairs["ValidTime"], seconds.to_string());
            assert_eq!(
                pairs["TemplateParam"],
                format!(r#"{{"code":"00001234","min":"{minutes}"}}"#)
            );
            assert!(!pairs.contains_key("RegionId"));
            assert!(!pairs.contains_key("SmsUpExtendCode"));
        }
        for seconds in [0, 59, i64::MAX.unsigned_abs() + 1, u64::MAX] {
            assert!(matches!(
                request(&client, seconds, context()),
                Err(BuildError::Validity)
            ));
        }
        assert!(request(&client, i64::MAX.unsigned_abs(), context()).is_ok());
    }

    #[test]
    fn accepts_uuid_nonce_values_without_version_or_variant_restrictions() {
        let client = client();
        for nonce in [
            Uuid::new_v4(),
            Uuid::nil(),
            Uuid::max(),
            "01890f6e-7b80-4000-8000-000000000001"
                .parse()
                .expect("v4 UUID parses"),
            "01890f6e-7b80-7000-7000-000000000001"
                .parse()
                .expect("non-RFC UUID parses"),
        ] {
            let mut context = context();
            context.nonce = nonce;
            let request = request(&client, 60, context).expect("UUID nonce is valid");
            assert_eq!(
                request.inner.headers()["x-acs-signature-nonce"],
                nonce.to_string()
            );
        }
    }

    #[test]
    fn rejects_invalid_signing_dates_without_echoing_values() {
        let client = client();
        for year in [0, -1, 10000] {
            let mut context = context();
            context.date = Utc
                .with_ymd_and_hms(year, 1, 1, 0, 0, 0)
                .single()
                .expect("Chrono supports test year");
            assert!(matches!(
                request(&client, 60, context),
                Err(BuildError::Date)
            ));
        }
        let mut context = context();
        context.date = context
            .date
            .with_nanosecond(123_456_789)
            .expect("fractional second is valid");
        assert_eq!(
            request(&client, 60, context)
                .expect("fractional date is valid")
                .inner
                .headers()["x-acs-date"],
            "2023-10-26T10:22:32Z"
        );
        assert_eq!(
            format!("{:?}", header_value("secret\nvalue")),
            "Err(Header)"
        );
    }

    #[test]
    fn redacts_request_debug_output() {
        let client = client();
        assert_eq!(
            format!("{:?}", request(&client, 60, context())),
            r#"Ok(Request("[redacted]"))"#
        );
    }
}
