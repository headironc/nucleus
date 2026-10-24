//! PNVS response decoding and outcome classification.

use std::fmt::{self, Debug, Formatter};

use reqwest::StatusCode;
use serde::{Deserialize, Deserializer, de};
use strum::EnumString;
use uuid::Uuid;

use crate::identity::message::Id;

#[derive(Deserialize)]
enum SuccessCode {
    #[serde(rename = "OK")]
    Ok,
}

fn deserialize_failure_code<'de, D>(deserializer: D) -> Result<String, D::Error>
where
    D: Deserializer<'de>,
{
    let code = String::deserialize(deserializer)?;
    if code == "OK" {
        return Err(de::Error::custom("unexpected PNVS failure code"));
    }
    Ok(code)
}

struct Flag<const EXPECTED: bool>;

impl Default for Flag<false> {
    fn default() -> Self {
        Self
    }
}

impl<'de, const EXPECTED: bool> Deserialize<'de> for Flag<EXPECTED> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? != EXPECTED {
            return Err(serde::de::Error::custom("unexpected PNVS success value"));
        }
        Ok(Self)
    }
}

#[derive(Deserialize)]
struct Model {
    #[serde(rename = "OutId")]
    message_id: Id,
    #[serde(rename = "BizId")]
    biz_id: String,
    #[serde(rename = "RequestId")]
    request_id: Uuid,
}

/// Observed or documented direct PNVS codes that establish rejection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, EnumString)]
pub enum FailureCode {
    #[strum(serialize = "ILLEGAL_MOBILE")]
    IllegalMobile,
    #[strum(serialize = "biz.FREQUENCY")]
    Frequency,
    #[strum(serialize = "isv.INVALID_PARAMETERS")]
    InvalidParameters,
    #[strum(serialize = "FUNCTION_NOT_OPENED")]
    FunctionNotOpened,
    #[strum(serialize = "isv.BUSINESS_LIMIT_CONTROL")]
    BusinessLimitControl,
}

/// Acceptance, rejection, or uncertainty from PNVS content or an HTTP attempt.
pub enum Outcome {
    Accepted(Accepted),
    Rejected(FailureCode),
    Unknown(Unknown),
}

impl Debug for Outcome {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Accepted(value) => Debug::fmt(value, formatter),
            Self::Rejected(code) => formatter.debug_tuple("Rejected").field(code).finish(),
            Self::Unknown(reason) => formatter.debug_tuple("Unknown").field(reason).finish(),
        }
    }
}

/// A complete supplier acceptance correlated with the original message.
///
/// ```compile_fail
/// use domain::identity::message::response::Accepted;
/// let accepted = Accepted { biz_id: String::new(), request_id: Default::default() };
/// ```
pub struct Accepted {
    biz_id: String,
    request_id: Uuid,
}

impl Accepted {
    pub fn biz_id(&self) -> &str {
        &self.biz_id
    }

    pub fn request_id(&self) -> Uuid {
        self.request_id
    }
}

impl Debug for Accepted {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Accepted")
            .field("biz_id", &"[redacted]")
            .field("request_id", &self.request_id)
            .finish()
    }
}

/// Why a PNVS attempt or its decoded content has no reliable outcome.
pub enum Unknown {
    Inconsistent,
    UnrecognizedCode(String),
    OutIdMismatch,
    HttpStatus,
    UnexpectedStatus(FailureCode),
}

impl Debug for Unknown {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Inconsistent => "Inconsistent",
            Self::UnrecognizedCode(_) => "UnrecognizedCode([redacted])",
            Self::OutIdMismatch => "OutIdMismatch",
            Self::HttpStatus => "HttpStatus",
            Self::UnexpectedStatus(code) => {
                return formatter
                    .debug_tuple("UnexpectedStatus")
                    .field(code)
                    .finish();
            }
        })
    }
}

/// The mutually exclusive success and failure bodies returned on HTTP 2xx.
#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Response {
    Success(Success),
    Failure(Failure),
}

impl Response {
    pub(super) fn check(self, message_id: Id, status: StatusCode) -> Outcome {
        match self {
            Self::Success(response) => response.check(message_id),
            Self::Failure(response) => response.check(status),
        }
    }
}

/// A successful body must include its complete message correlation model.
#[derive(Deserialize)]
pub(super) struct Success {
    #[serde(rename = "Success")]
    success: Flag<true>,
    #[serde(rename = "Code")]
    code: SuccessCode,
    #[serde(rename = "Model")]
    model: Model,
}

impl Success {
    /// Check that the accepted message matches the original request.
    pub(super) fn check(self, message_id: Id) -> Outcome {
        let Self {
            success: Flag,
            code: SuccessCode::Ok,
            model,
        } = self;
        if model.message_id != message_id {
            return Outcome::Unknown(Unknown::OutIdMismatch);
        }
        if model.biz_id.trim().is_empty() {
            return Outcome::Unknown(Unknown::Inconsistent);
        }
        Outcome::Accepted(Accepted {
            biz_id: model.biz_id,
            request_id: model.request_id,
        })
    }
}

/// A failure body received on HTTP 2xx or 4xx.
#[derive(Deserialize)]
pub(super) struct Failure {
    #[serde(rename = "Success", default)]
    success: Flag<false>,
    #[serde(rename = "Code", deserialize_with = "deserialize_failure_code")]
    code: String,
}

impl Failure {
    /// Known rejections are accepted on HTTP 2xx or 400.
    pub(super) fn check(self, status: StatusCode) -> Outcome {
        let Self {
            success: Flag,
            code,
        } = self;
        match code.parse() {
            Ok(code) if status.is_success() || status == StatusCode::BAD_REQUEST => {
                Outcome::Rejected(code)
            }
            Ok(code) => Outcome::Unknown(Unknown::UnexpectedStatus(code)),
            Err(_) => Outcome::Unknown(Unknown::UnrecognizedCode(code)),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use reqwest::StatusCode;

    use crate::identity::message::{
        Id,
        response::{Failure, FailureCode, Outcome, Response, Success, Unknown},
    };

    const MESSAGE_ID: &str = "01890f6e-7b80-7000-8000-000000000000";

    fn original_id() -> Id {
        MESSAGE_ID.parse().expect("fixed Message ID is valid")
    }

    fn decoded_response(json: &str) -> Success {
        serde_json::from_str(json).expect("fixed PNVS response is valid JSON")
    }

    fn decoded_failure(json: &str) -> Failure {
        serde_json::from_str(json).expect("fixed PNVS failure is valid JSON")
    }

    #[test]
    fn accepts_only_a_complete_success_for_the_original_message() {
        let body = r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#;

        assert!(matches!(
            decoded_response(body).check(original_id()),
            Outcome::Accepted(accepted)
                if accepted.biz_id() == "supplier-business-id"
                    && accepted.request_id().to_string() == "a3671ccf-0102-4c8e-8797-a3678e091d09"
        ));
    }

    #[test]
    fn recognizes_observed_and_documented_rejection_codes() {
        for (code, expected) in [
            ("ILLEGAL_MOBILE", FailureCode::IllegalMobile),
            ("biz.FREQUENCY", FailureCode::Frequency),
            ("isv.INVALID_PARAMETERS", FailureCode::InvalidParameters),
            ("FUNCTION_NOT_OPENED", FailureCode::FunctionNotOpened),
            (
                "isv.BUSINESS_LIMIT_CONTROL",
                FailureCode::BusinessLimitControl,
            ),
        ] {
            let body = format!(r#"{{"Code":"{code}","Success":false}}"#);
            assert!(matches!(
                decoded_failure(&body).check(StatusCode::BAD_REQUEST),
                Outcome::Rejected(actual) if actual == expected
            ));
        }
        assert!(matches!(
            decoded_failure(r#"{"Code":"biz.FREQUENCY","Success":false}"#)
                .check(StatusCode::BAD_REQUEST),
            Outcome::Rejected(FailureCode::Frequency)
        ));
    }

    #[test]
    fn correlates_out_id_by_uuid_value() {
        for original in [
            "550e8400-e29b-41d4-a716-446655440000",
            "01890f6e-7b80-7000-8000-000000000000",
            "01890f6e-7b80-7000-c000-000000000000",
            "00000000-0000-0000-0000-000000000000",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            let message_id = original.parse().expect("fixed message ID");
            for returned in [
                original.to_uppercase(),
                original.replace('-', ""),
                format!("{{{original}}}"),
                format!("urn:uuid:{original}"),
            ] {
                let response = format!(
                    r#"{{"Code":"OK","Success":true,"Model":{{"OutId":"{returned}","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}}}"#
                );
                assert!(matches!(
                    decoded_response(&response).check(message_id),
                    Outcome::Accepted(_)
                ));
                let different = "550e8400-e29b-41d4-a716-446655440001"
                    .parse()
                    .expect("different message ID");
                assert!(matches!(
                    decoded_response(&response).check(different),
                    Outcome::Unknown(Unknown::OutIdMismatch)
                ));
            }
        }
    }

    #[test]
    fn treats_unreliable_direct_responses_as_unknown() {
        assert!(matches!(
            decoded_response(r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":" ","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#)
                .check(original_id()),
            Outcome::Unknown(Unknown::Inconsistent)
        ));
        assert!(matches!(
            decoded_response(r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000001","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#)
                .check(original_id()),
            Outcome::Unknown(Unknown::OutIdMismatch)
        ));
        assert!(matches!(
            decoded_failure(r#"{"Code":"NEW_CODE","Success":false}"#).check(StatusCode::BAD_REQUEST),
            Outcome::Unknown(Unknown::UnrecognizedCode(code)) if code == "NEW_CODE"
        ));
    }

    #[test]
    fn accepts_absent_failure_flag_and_retains_public_error_codes() {
        for code in [
            "SignatureDoesNotMatch",
            "MissingPhoneNumber",
            "InvalidTimeStamp.Expired",
        ] {
            let body = format!(
                r#"{{"Code":"{code}","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09","Message":"supplier failure","HostId":"dypnsapi.aliyuncs.com","Recommend":"https://example.invalid/"}}"#
            );
            assert!(matches!(
                decoded_failure(&body).check(StatusCode::BAD_REQUEST),
                Outcome::Unknown(Unknown::UnrecognizedCode(actual)) if actual == code
            ));
        }
        assert!(matches!(
            decoded_failure(r#"{"Code":"biz.FREQUENCY"}"#).check(StatusCode::BAD_REQUEST),
            Outcome::Rejected(FailureCode::Frequency)
        ));
    }

    #[test]
    fn rejects_malformed_failure_bodies() {
        for body in [
            r#"{"Code":"OK","Success":false}"#,
            r#"{"Code":"biz.FREQUENCY","Success":true}"#,
            r#"{"Success":false}"#,
            r#"{"Code":null,"Success":false}"#,
            r#"{"Code":"biz.FREQUENCY","Success":null}"#,
            r#"{"Code":"biz.FREQUENCY","Success":"false"}"#,
            r#"{"Code":"biz.FREQUENCY","Success":0}"#,
            r#"not-json"#,
        ] {
            assert!(serde_json::from_str::<Failure>(body).is_err());
        }
    }

    #[test]
    fn rejects_malformed_supplier_responses_during_deserialization() {
        for body in [
            r#"{"Code":"OK","Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#,
            r#"{"Code":"biz.FREQUENCY","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#,
            r#"{"Code":"OK","Success":false}"#,
            r#"{"Code":"OK","Success":true}"#,
            r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id"}}"#,
            r#"{"Code":"OK","Success":true,"Model":{"OutId":"bad","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#,
            r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"bad"}}"#,
            r#"not-json"#,
        ] {
            assert!(serde_json::from_str::<Success>(body).is_err(), "{body}");
        }
    }

    #[test]
    fn decodes_both_2xx_shapes_without_accepting_malformed_success() {
        let success = r#"{"Code":"OK","Success":true,"Model":{"OutId":"01890f6e-7b80-7000-8000-000000000000","BizId":"supplier-business-id","RequestId":"a3671ccf-0102-4c8e-8797-a3678e091d09"}}"#;
        let response: Response = serde_json::from_str(success).expect("complete success");
        assert!(matches!(
            response.check(original_id(), StatusCode::OK),
            Outcome::Accepted(_)
        ));
        for body in [
            r#"{"Success":false,"Code":"ILLEGAL_MOBILE"}"#,
            r#"{"Code":"ILLEGAL_MOBILE"}"#,
        ] {
            let response: Response = serde_json::from_str(body).expect("valid failure");
            assert!(matches!(
                response.check(original_id(), StatusCode::OK),
                Outcome::Rejected(FailureCode::IllegalMobile)
            ));
        }
        for body in [
            r#"{"Success":true,"Code":"OK"}"#,
            r#"{"Success":true,"Code":"OK","Model":null}"#,
            r#"{"Success":true,"Code":"ILLEGAL_MOBILE"}"#,
            r#"{"Success":false,"Code":"OK"}"#,
            r#"{"Code":"OK"}"#,
            r#"{"Success":null,"Code":"ILLEGAL_MOBILE"}"#,
            r#"{"Success":"false","Code":"ILLEGAL_MOBILE"}"#,
        ] {
            assert!(serde_json::from_str::<Response>(body).is_err());
        }
    }
}
