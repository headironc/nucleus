//! Validated request settings and the message client's call budget.

use std::time::Duration;

use derive_more::AsRef;
use thiserror::Error;

use crate::identity::challenge::Purpose;

pub const DEFAULT_TIMEOUT_MILLISECONDS: u64 = 10000;

/// The deployment's selected signature/templates and call budget.
#[derive(Debug)]
pub struct Settings {
    signature: Signature,
    template: Templates,
    timeout: Duration,
}

impl Settings {
    /// Assemble validated supplier values with a checked timeout budget.
    pub fn new(
        signature: Signature,
        template: Templates,
        timeout_milliseconds: u64,
    ) -> Result<Self, TimeoutError> {
        if !(1000..=10000).contains(&timeout_milliseconds) {
            return Err(TimeoutError);
        }
        Ok(Self {
            signature,
            template,
            timeout: Duration::from_millis(timeout_milliseconds),
        })
    }

    pub(super) fn signature(&self) -> &str {
        self.signature.as_ref()
    }
    pub(super) fn code(&self, purpose: Purpose) -> &str {
        match purpose {
            Purpose::Authentication => self.template.authentication.as_ref(),
            Purpose::MobileChange => self.template.mobile_change.as_ref(),
            Purpose::MobileBinding => self.template.mobile_binding.as_ref(),
        }
    }
    pub(super) fn timeout(&self) -> Duration {
        self.timeout
    }
}

#[derive(Debug, Error)]
#[error("PNVS timeout must be 1000 to 10000 milliseconds")]
pub struct TimeoutError;

/// The three required supplier template codes, sharing one signature.
#[derive(Debug)]
pub struct Templates {
    authentication: TemplateCode,
    mobile_change: TemplateCode,
    mobile_binding: TemplateCode,
}

impl Templates {
    pub fn new(
        authentication: TemplateCode,
        mobile_change: TemplateCode,
        mobile_binding: TemplateCode,
    ) -> Self {
        Self {
            authentication,
            mobile_change,
            mobile_binding,
        }
    }
}

/// A supplier SMS signature, distinct from an HMAC signature.
#[derive(Debug, AsRef)]
#[as_ref(str)]
pub struct Signature(String);

impl Signature {
    /// Accept nonempty text without control characters.
    pub fn new(value: String) -> Result<Self, SignatureError> {
        if value.is_empty() || value.chars().any(char::is_control) {
            return Err(SignatureError);
        }
        Ok(Self(value))
    }
}

/// A supplier SMS signature failed local validation.
#[derive(Debug, Error)]
#[error("PNVS signature must be nonempty without control characters")]
pub struct SignatureError;

/// A supplier template identifier, distinct from the verification code.
#[derive(Debug, AsRef)]
#[as_ref(str)]
pub struct TemplateCode(String);

impl TemplateCode {
    /// Accept nonempty text without whitespace or control characters.
    pub fn new(value: String) -> Result<Self, TemplateCodeError> {
        if value.is_empty()
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(TemplateCodeError);
        }
        Ok(Self(value))
    }
}

/// A template identifier failed local validation.
#[derive(Debug, Error)]
#[error("PNVS template code must be nonempty without whitespace or control characters")]
pub struct TemplateCodeError;

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use crate::identity::message::settings::*;

    fn settings(milliseconds: u64) -> Result<Settings, TimeoutError> {
        Settings::new(
            Signature::new("signature".into()).expect("valid signature"),
            Templates::new(
                TemplateCode::new("authentication".into()).expect("valid template"),
                TemplateCode::new("change".into()).expect("valid template"),
                TemplateCode::new("binding".into()).expect("valid template"),
            ),
            milliseconds,
        )
    }

    #[test]
    fn validates_timeout_boundaries() {
        for milliseconds in [1000, 1001, 10000] {
            let settings = settings(milliseconds).expect("valid timeout");
            assert_eq!(settings.timeout(), Duration::from_millis(milliseconds));
        }
        for milliseconds in [0, 999, 10001, u64::MAX] {
            assert!(matches!(settings(milliseconds), Err(TimeoutError)));
        }
    }

    #[test]
    fn preserves_signature_and_checks_its_boundaries() {
        for signature in ["", "signature\n", "signature\0"] {
            assert!(Signature::new(signature.into()).is_err());
        }
        assert_eq!(
            Signature::new(" signature ".into())
                .expect("valid signature")
                .as_ref(),
            " signature "
        );
        let settings = settings(DEFAULT_TIMEOUT_MILLISECONDS).expect("valid settings");
        assert_eq!(settings.signature(), "signature");
    }

    #[test]
    fn selects_the_template_code_for_each_purpose() {
        let settings = settings(DEFAULT_TIMEOUT_MILLISECONDS).expect("valid settings");
        assert_eq!(settings.code(Purpose::Authentication), "authentication");
        assert_eq!(settings.code(Purpose::MobileChange), "change");
        assert_eq!(settings.code(Purpose::MobileBinding), "binding");
    }

    #[test]
    fn rejects_invalid_template_code_without_echoing_it() {
        for invalid in [
            "",
            " sensitive-template",
            "sensitive-template\n",
            "sensitive-template\0",
            "sensitive-template\u{2003}",
        ] {
            let error = TemplateCode::new(invalid.into()).expect_err("invalid template");
            assert!(!format!("{error:?} {error}").contains("sensitive-template"));
        }
    }
}
