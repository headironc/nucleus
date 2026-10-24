//! Siteverify response checks and database-time proof acceptance.

use std::fmt::{self, Formatter};

use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Deserializer, de};
use serde_with::DeserializeFromStr;
use strum::EnumString;
use thiserror::Error;
use url::Host;

use crate::identity::{
    challenge::{Purpose, settings::AllowedHostnames},
    operation,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
pub(super) enum Action {
    #[serde(rename = "send_authentication_code")]
    Authentication,
    #[serde(rename = "send_mobile_change_code")]
    MobileChange,
    #[serde(rename = "send_mobile_binding_code")]
    MobileBinding,
    #[serde(other)]
    Unknown,
}

impl From<Purpose> for Action {
    fn from(purpose: Purpose) -> Self {
        match purpose {
            Purpose::Authentication => Self::Authentication,
            Purpose::MobileChange => Self::MobileChange,
            Purpose::MobileBinding => Self::MobileBinding,
        }
    }
}

/// A Siteverify error code; unknown values remain available without diagnostic disclosure.
#[derive(Clone, PartialEq, Eq, DeserializeFromStr, EnumString)]
#[strum(serialize_all = "kebab-case")]
pub enum Code {
    MissingInputSecret,
    InvalidInputSecret,
    MissingInputResponse,
    InvalidInputResponse,
    BadRequest,
    TimeoutOrDuplicate,
    InternalError,
    #[strum(default)]
    Unknown(String),
}

impl fmt::Debug for Code {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::MissingInputSecret => "MissingInputSecret",
            Self::InvalidInputSecret => "InvalidInputSecret",
            Self::MissingInputResponse => "MissingInputResponse",
            Self::InvalidInputResponse => "InvalidInputResponse",
            Self::BadRequest => "BadRequest",
            Self::TimeoutOrDuplicate => "TimeoutOrDuplicate",
            Self::InternalError => "InternalError",
            Self::Unknown(_) => "Unknown([redacted])",
        })
    }
}

#[derive(Deserialize)]
#[serde(untagged)]
pub(super) enum Response {
    Success {
        #[serde(rename = "success")]
        success: Flag<true>,
        #[serde(rename = "challenge_ts")]
        solved_at: DateTime<Utc>,
        #[serde(deserialize_with = "deserialize_hostname")]
        hostname: Host,
        action: Action,
        #[serde(rename = "cdata")]
        operation_id: operation::Id,
        #[serde(rename = "error-codes", default)]
        error_codes: Vec<Code>,
    },
    Failure {
        #[serde(rename = "success")]
        success: Flag<false>,
        #[serde(rename = "error-codes")]
        error_codes: Vec<Code>,
    },
}

impl Response {
    /// Match the supplier result against server-owned request context.
    pub(super) fn check(
        self,
        purpose: Purpose,
        operation_id: operation::Id,
        allowed_hostnames: &AllowedHostnames,
    ) -> Context {
        match self {
            Self::Failure {
                success: Flag,
                error_codes,
            } => Context::SupplierFailed { error_codes },
            Self::Success {
                success: Flag,
                solved_at,
                hostname,
                action,
                operation_id: returned_id,
                error_codes,
            } => {
                drop(error_codes);
                if !allowed_hostnames.contains(&hostname)
                    || action != purpose.into()
                    || returned_id != operation_id
                {
                    Context::Mismatch
                } else {
                    Context::Matched(Matched { solved_at })
                }
            }
        }
    }
}

pub(super) struct Flag<const EXPECTED: bool>;

impl<'de, const EXPECTED: bool> Deserialize<'de> for Flag<EXPECTED> {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        if bool::deserialize(deserializer)? != EXPECTED {
            return Err(de::Error::custom("unexpected Siteverify success value"));
        }
        Ok(Self)
    }
}

fn deserialize_hostname<'de, D>(deserializer: D) -> Result<Host, D::Error>
where
    D: Deserializer<'de>,
{
    let value = String::deserialize(deserializer)?;
    Host::parse(&value).map_err(de::Error::custom)
}

/// The result of checking a Siteverify response before its database-time check.
pub enum Context {
    SupplierFailed { error_codes: Vec<Code> },
    Mismatch,
    Matched(Matched),
}

impl Context {
    /// Accept a matched supplier result at the database time of the write.
    pub fn accept(&self, finished_at: DateTime<Utc>) -> Result<Proof, Error> {
        let solved_at = match self {
            Self::SupplierFailed { error_codes } => {
                return Err(Error::SupplierFailed {
                    error_codes: error_codes.clone(),
                });
            }
            Self::Mismatch => return Err(Error::Mismatch),
            Self::Matched(Matched { solved_at }) => *solved_at,
        };

        if solved_at.signed_duration_since(finished_at) > TimeDelta::seconds(2) {
            return Err(Error::TimestampInvalid);
        }

        let start = solved_at.min(finished_at);
        let expires_at = match start.checked_add_signed(TimeDelta::seconds(300)) {
            Some(expires_at) => expires_at,
            None => return Err(Error::TimeOutOfRange),
        };
        if finished_at >= expires_at {
            return Err(Error::Expired);
        }

        Ok(Proof {
            solved_at,
            finished_at,
            expires_at,
        })
    }
}

/// A supplier result whose hostname, action and operation have been checked.
///
/// ```compile_fail
/// use domain::identity::challenge::response::Matched;
/// let matched = Matched { solved_at: Default::default() };
/// ```
pub struct Matched {
    solved_at: DateTime<Utc>,
}

/// The accepted Siteverify proof and its derived deadline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Proof {
    solved_at: DateTime<Utc>,
    finished_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
}

impl Proof {
    /// Return the supplier's completion time.
    pub fn solved_at(self) -> DateTime<Utc> {
        self.solved_at
    }

    /// Return the database time at which the proof was accepted.
    pub fn finished_at(self) -> DateTime<Utc> {
        self.finished_at
    }

    /// Return the deadline derived from the earlier completion time.
    pub fn expires_at(self) -> DateTime<Utc> {
        self.expires_at
    }
}

/// Why a Siteverify context cannot become an accepted proof.
#[derive(Error)]
pub enum Error {
    #[error("siteverify supplier rejected the token")]
    SupplierFailed { error_codes: Vec<Code> },
    #[error("siteverify context does not match the request")]
    Mismatch,
    #[error("siteverify completion time is too far in the future")]
    TimestampInvalid,
    #[error("siteverify proof has expired")]
    Expired,
    #[error("siteverify proof deadline is outside the supported time range")]
    TimeOutOfRange,
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::SupplierFailed { .. } => formatter
                .debug_struct("SupplierFailed")
                .field("error_codes", &"[redacted]")
                .finish(),
            Self::Mismatch => formatter.write_str("Mismatch"),
            Self::TimestampInvalid => formatter.write_str("TimestampInvalid"),
            Self::Expired => formatter.write_str("Expired"),
            Self::TimeOutOfRange => formatter.write_str("TimeOutOfRange"),
        }
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use nonempty::NonEmpty;

    use crate::identity::challenge::{
        Purpose,
        response::{Code, Context, Error, Matched, Response},
        settings::AllowedHostnames,
    };
    const OPERATION_ID: &str = "01890f6e-7b80-7000-8000-000000000000";

    fn decoded_response(json: &str) -> Response {
        serde_json::from_str(json).expect("fixed Siteverify response is valid JSON")
    }

    fn allowed_hostnames() -> AllowedHostnames {
        AllowedHostnames::new(NonEmpty::new("example.com".to_owned()))
            .expect("fixed hostname allowlist is nonempty")
    }

    fn fixed_time() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2022-02-28T15:14:30.096Z")
            .expect("fixed timestamp is valid")
            .with_timezone(&Utc)
    }

    #[test]
    fn retained_response_is_checked_against_each_write_time() {
        let context = Context::Matched(Matched {
            solved_at: fixed_time(),
        });
        let first = context.accept(fixed_time()).expect("first write");
        let later = context
            .accept(fixed_time() + TimeDelta::seconds(5))
            .expect("recovery write");
        assert_eq!(first.solved_at(), later.solved_at());
        assert_eq!(first.expires_at(), later.expires_at());
        assert_ne!(first.finished_at(), later.finished_at());
        assert!(matches!(
            context.accept(first.expires_at()),
            Err(Error::Expired)
        ));
    }

    #[test]
    fn supplier_failure_diagnostics_hide_codes_without_discarding_them() {
        let error =
            decoded_response(r#"{"success":false,"error-codes":["sensitive-supplier-text"]}"#)
                .check(
                    Purpose::Authentication,
                    OPERATION_ID.parse().expect("operation ID"),
                    &allowed_hostnames(),
                )
                .accept(fixed_time())
                .expect_err("supplier rejection cannot yield a proof");
        assert!(!format!("{error:?} {error}").contains("sensitive-supplier-text"));
        let Error::SupplierFailed { error_codes } = error else {
            panic!("supplier failure retains its codes");
        };
        assert!(!format!("{error_codes:?}").contains("sensitive-supplier-text"));
        assert_eq!(
            error_codes,
            [Code::Unknown("sensitive-supplier-text".to_owned())]
        );
    }

    #[test]
    fn decodes_documented_codes_and_preserves_unrecognized_text() {
        let response = decoded_response(
            r#"{"success":false,"error-codes":["missing-input-secret","invalid-input-secret","missing-input-response","invalid-input-response","bad-request","timeout-or-duplicate","internal-error","INTERNAL-ERROR"," internal-error ","new-code",""]}"#,
        );
        let Context::SupplierFailed { error_codes } = response.check(
            Purpose::Authentication,
            OPERATION_ID.parse().expect("operation ID"),
            &allowed_hostnames(),
        ) else {
            panic!("supplier failure retains its codes");
        };
        assert_eq!(
            error_codes,
            [
                Code::MissingInputSecret,
                Code::InvalidInputSecret,
                Code::MissingInputResponse,
                Code::InvalidInputResponse,
                Code::BadRequest,
                Code::TimeoutOrDuplicate,
                Code::InternalError,
                Code::Unknown("INTERNAL-ERROR".into()),
                Code::Unknown(" internal-error ".into()),
                Code::Unknown("new-code".into()),
                Code::Unknown(String::new()),
            ]
        );
    }

    #[test]
    fn matches_success_context_without_accepting_the_proof() {
        let operation_id = OPERATION_ID.parse().expect("fixed Operation ID is valid");
        let response = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000","metadata":{"ephemeral_id":"ignored"}}"#;

        let context = decoded_response(response).check(
            Purpose::Authentication,
            operation_id,
            &allowed_hostnames(),
        );
        assert!(matches!(
            context,
            Context::Matched(Matched { solved_at }) if solved_at.timestamp_millis() == 1_646_061_270_096
        ));
    }

    #[test]
    fn keeps_supplier_failure_codes_without_success_fields() {
        let operation_id = OPERATION_ID.parse().expect("fixed Operation ID is valid");
        let response = r#"{"success":false,"error-codes":["invalid-input-response"]}"#;

        let context = decoded_response(response).check(
            Purpose::Authentication,
            operation_id,
            &allowed_hostnames(),
        );
        assert!(matches!(
            context,
            Context::SupplierFailed { error_codes }
                if error_codes == [Code::InvalidInputResponse]
        ));
    }

    #[test]
    fn correlates_cdata_by_uuid_value() {
        for original in [
            "550e8400-e29b-41d4-a716-446655440000",
            "01890f6e-7b80-7000-8000-000000000000",
            "01890f6e-7b80-7000-c000-000000000000",
            "00000000-0000-0000-0000-000000000000",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            let operation_id = original.parse().expect("fixed operation ID");
            for returned in [
                original.to_uppercase(),
                original.replace('-', ""),
                format!("{{{original}}}"),
                format!("urn:uuid:{original}"),
            ] {
                let response = format!(
                    r#"{{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"{returned}"}}"#
                );
                assert!(matches!(
                    decoded_response(&response).check(
                        Purpose::Authentication,
                        operation_id,
                        &allowed_hostnames()
                    ),
                    Context::Matched(_)
                ));
                let different = "550e8400-e29b-41d4-a716-446655440001"
                    .parse()
                    .expect("different operation ID");
                assert!(matches!(
                    decoded_response(&response).check(
                        Purpose::Authentication,
                        different,
                        &allowed_hostnames()
                    ),
                    Context::Mismatch
                ));
            }
        }
    }

    #[test]
    fn rejects_each_mismatched_success_context() {
        let operation_id = OPERATION_ID.parse().expect("fixed Operation ID is valid");
        for response in [
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"other.example","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_mobile_change_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"other_action","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000001"}"#,
        ] {
            assert!(matches!(
                decoded_response(response).check(
                    Purpose::Authentication,
                    operation_id,
                    &allowed_hostnames()
                ),
                Context::Mismatch
            ));
        }
    }

    #[test]
    fn matches_normalized_url_hosts() {
        let operation_id = OPERATION_ID.parse().expect("fixed Operation ID is valid");
        let allowed = AllowedHostnames::new(NonEmpty::new("EXAMPLE.COM".to_owned()))
            .expect("fixed hostname is valid");
        let response = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#;

        assert!(matches!(
            decoded_response(response).check(Purpose::Authentication, operation_id, &allowed),
            Context::Matched(_)
        ));
    }

    #[test]
    fn rejects_missing_null_or_invalid_required_response_fields_during_deserialization() {
        for response in [
            r#"{"success":false}"#,
            r#"{"success":false,"error-codes":null}"#,
            r#"{"success":false,"error-codes":"internal-error"}"#,
            r#"{"success":false,"error-codes":[null]}"#,
            r#"{"success":false,"error-codes":[42]}"#,
            r#"{"success":false,"error-codes":[true]}"#,
            r#"{"success":false,"error-codes":[{"internal-error":null}]}"#,
            r#"{"success":true,"error-codes":[]}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000","error-codes":null}"#,
            r#"{"success":true,"hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":null,"hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":null,"cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":null}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":""}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"not-a-uuid"}"#,
            r#"{"success":true,"challenge_ts":"not-a-time","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":42,"action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"https://example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000"}"#,
            r#"{}"#,
            r#"{"success":true,"success":false}"#,
        ] {
            assert!(
                serde_json::from_str::<Response>(response).is_err(),
                "{response}"
            );
        }
    }

    #[test]
    fn accepts_success_error_codes_and_failure_without_success_fields() {
        let operation_id = OPERATION_ID.parse().expect("fixed Operation ID is valid");
        let success = r#"{"success":true,"challenge_ts":"2022-02-28T15:14:30.096Z","hostname":"example.com","action":"send_authentication_code","cdata":"01890f6e-7b80-7000-8000-000000000000","error-codes":[]}"#;
        let failure = r#"{"success":false,"error-codes":[],"hostname":"example.com"}"#;

        assert!(matches!(
            decoded_response(success).check(
                Purpose::Authentication,
                operation_id,
                &allowed_hostnames()
            ),
            Context::Matched(_)
        ));
        assert!(matches!(
            decoded_response(failure).check(Purpose::Authentication, operation_id, &allowed_hostnames()),
            Context::SupplierFailed { error_codes } if error_codes.is_empty()
        ));
    }

    #[test]
    fn accepts_timestamp_at_the_future_tolerance_boundary() {
        let finished_at = fixed_time();
        let solved_at = finished_at + TimeDelta::seconds(2);
        let proof = Context::Matched(Matched { solved_at })
            .accept(finished_at)
            .expect("two seconds of positive clock tolerance is allowed");

        assert_eq!(proof.solved_at(), solved_at);
        assert_eq!(proof.finished_at(), finished_at);
        assert_eq!(proof.expires_at(), finished_at + TimeDelta::seconds(300));
        assert!(matches!(
            (Context::Matched(Matched {
                solved_at: solved_at + TimeDelta::milliseconds(1)
            }))
            .accept(finished_at),
            Err(Error::TimestampInvalid)
        ));
    }

    #[test]
    fn expires_at_the_earlier_completion_time_plus_five_minutes() {
        let finished_at = fixed_time();
        let solved_at = finished_at - TimeDelta::seconds(300) + TimeDelta::milliseconds(1);
        let proof = Context::Matched(Matched { solved_at })
            .accept(finished_at)
            .expect("one millisecond remains before expiry");

        assert_eq!(proof.expires_at(), finished_at + TimeDelta::milliseconds(1));
        assert!(matches!(
            (Context::Matched(Matched {
                solved_at: finished_at - TimeDelta::seconds(300)
            }))
            .accept(finished_at),
            Err(Error::Expired)
        ));
    }

    #[test]
    fn supplier_failure_context_mismatch_and_time_overflow_do_not_create_proofs() {
        let finished_at = fixed_time();
        assert!(matches!(
            (Context::SupplierFailed {
                error_codes: vec![Code::InvalidInputResponse],
            })
            .accept(finished_at),
            Err(Error::SupplierFailed { error_codes })
                if error_codes == [Code::InvalidInputResponse]
        ));
        assert!(matches!(
            Context::Mismatch.accept(finished_at),
            Err(Error::Mismatch)
        ));
        assert!(matches!(
            (Context::Matched(Matched {
                solved_at: DateTime::<Utc>::MAX_UTC
            }))
            .accept(DateTime::<Utc>::MAX_UTC),
            Err(Error::TimeOutOfRange)
        ));
    }
}
