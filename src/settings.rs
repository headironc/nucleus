//! Non-secret deployment settings loaded from exported process variables.

use std::{fmt, net::SocketAddr, time::Duration};

use config::{Config, Environment};
use domain::identity::{authentication, challenge, message};
use ipnet::IpNet;
use nonempty::NonEmpty;
use platform::database;
use serde::Deserialize;
use serde_with::{DisplayFromStr, json::JsonString, serde_as};
use thiserror::Error;

/// Validated settings for the delivered capabilities.
pub struct Settings {
    pub(crate) database: crate::database::Settings,
    pub(crate) challenge: challenge::settings::Settings,
    pub(crate) message: message::settings::Settings,
    pub(crate) listen: SocketAddr,
    pub(crate) source: crate::source::Settings,
    pub(crate) requests: crate::identity::Settings,
    pub(crate) services: domain::startup::Settings,
    pub(crate) process: crate::process::Settings,
}

impl Settings {
    /// Read exported variables without loading files or constructing clients.
    pub fn load() -> Result<Self, Error> {
        Input::load(Environment::default())?.try_into()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    database: database::assembly::input::Input,
    challenge: Challenge,
    message: Message,
    http: Http,
    identity: Identity,
    wechat: Wechat,
    #[serde(default)]
    process: Process,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Http {
    #[serde_as(as = "DisplayFromStr")]
    listen: SocketAddr,
    origin: String,
    #[serde_as(as = "JsonString<Vec<DisplayFromStr>>")]
    trusted_proxies: Vec<IpNet>,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Identity {
    authentication: Authentication,
    avatar: Avatar,
    #[serde(default)]
    message: Limits,
    #[serde(default)]
    session: Session,
    #[serde(default)]
    registration: Registration,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "default_recovery")]
    authentication_recovery_seconds: u64,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Avatar {
    origin: String,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "default_avatar_lifetime")]
    lifetime_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    #[serde(default = "default_avatar_retention")]
    retention_seconds: u64,
}

fn default_avatar_lifetime() -> u64 {
    domain::identity::avatar::link::Link::default_lifetime().as_secs()
}

fn default_avatar_retention() -> u64 {
    domain::identity::avatar::retention::Settings::default().seconds()
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Wechat {
    website: Website,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Website {
    app_id: String,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Session {
    #[serde_as(as = "DisplayFromStr")]
    message_seconds: u64,
}
impl Default for Session {
    fn default() -> Self {
        Self {
            message_seconds: domain::identity::session::settings::DEFAULT_SESSION_SECONDS,
        }
    }
}
#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Registration {
    #[serde_as(as = "DisplayFromStr")]
    lifetime_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    hourly: usize,
}
impl Default for Registration {
    fn default() -> Self {
        Self {
            lifetime_seconds:
                domain::identity::authentication::session::settings::DEFAULT_REGISTRATION_SECONDS,
            hourly: domain::registration::Limits::default().hourly(),
        }
    }
}
fn default_recovery() -> u64 {
    domain::identity::session::settings::DEFAULT_RECOVERY_SECONDS
}
#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Authentication {
    #[serde_as(as = "DisplayFromStr")]
    capacity: usize,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Limits {
    #[serde_as(as = "DisplayFromStr")]
    lifetime_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    daily: usize,
    mobile: Mobile,
    source: Source,
}

impl Default for Limits {
    fn default() -> Self {
        let defaults = message::Limits::default();
        Self {
            lifetime_seconds: defaults.lifetime().num_seconds().unsigned_abs(),
            daily: defaults.daily(),
            mobile: Mobile::default(),
            source: Source::default(),
        }
    }
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Mobile {
    #[serde_as(as = "DisplayFromStr")]
    interval_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    hourly: usize,
    #[serde_as(as = "DisplayFromStr")]
    daily: usize,
}

impl Default for Mobile {
    fn default() -> Self {
        let defaults = message::limits::Mobile::default();
        Self {
            interval_seconds: defaults.interval().num_seconds().unsigned_abs(),
            hourly: defaults.hourly(),
            daily: defaults.daily(),
        }
    }
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Source {
    #[serde_as(as = "DisplayFromStr")]
    minute: usize,
    #[serde_as(as = "DisplayFromStr")]
    hourly: usize,
}

impl Default for Source {
    fn default() -> Self {
        let defaults = message::limits::Source::default();
        Self {
            minute: defaults.minute(),
            hourly: defaults.hourly(),
        }
    }
}

impl Identity {
    fn parse(
        self,
        app_id: domain::identity::website::AppId,
    ) -> Result<domain::startup::Settings, Error> {
        let input = self;
        let mobile = message::limits::Mobile::new(
            input.message.mobile.interval_seconds,
            input.message.mobile.hourly,
            input.message.mobile.daily,
        )?;
        let source =
            message::limits::Source::new(input.message.source.minute, input.message.source.hourly)?;
        let limits = message::Limits::new(
            input.message.lifetime_seconds,
            input.message.daily,
            mobile,
            source,
        )?;
        let session = authentication::session::settings::Settings::new(
            app_id,
            input.session.message_seconds,
            input.registration.lifetime_seconds,
            input.authentication_recovery_seconds,
        )?;
        Ok(domain::startup::Settings::new(
            input.authentication.capacity,
            limits,
            session,
            domain::registration::Limits::new(input.registration.hourly)?,
            domain::identity::avatar::link::Link::new(
                input.avatar.origin,
                Duration::from_secs(input.avatar.lifetime_seconds),
            )?,
        )?
        .with_avatar_retention(domain::identity::avatar::retention::Settings::new(
            input.avatar.retention_seconds,
        )?))
    }
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Process {
    #[serde_as(as = "DisplayFromStr")]
    stop_seconds: u64,
}

impl Default for Process {
    fn default() -> Self {
        Self {
            stop_seconds: crate::process::DEFAULT_STOP_SECONDS,
        }
    }
}

impl Input {
    fn load(source: Environment) -> Result<Self, Error> {
        let source = source
            .prefix("NUCLEUS")
            .prefix_separator("_")
            .separator("__")
            .try_parsing(false)
            .ignore_empty(false);
        let config = Config::builder().add_source(source).build()?;
        Ok(serde_path_to_error::deserialize(config)?)
    }
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    site_key: String,
    #[serde_as(as = "JsonString")]
    hostnames: Vec<String>,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    signature: String,
    template: Templates,
    #[serde(default = "default_timeout")]
    #[serde_as(as = "DisplayFromStr")]
    timeout_milliseconds: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Templates {
    authentication: String,
    mobile_change: String,
    mobile_binding: String,
}

fn default_timeout() -> u64 {
    message::settings::DEFAULT_TIMEOUT_MILLISECONDS
}

impl TryFrom<Input> for Settings {
    type Error = Error;

    fn try_from(input: Input) -> Result<Self, Error> {
        Ok(Self {
            database: input.database.try_into()?,
            challenge: challenge::settings::Settings::new(
                challenge::settings::AllowedHostnames::new(
                    NonEmpty::from_vec(input.challenge.hostnames).ok_or(Error::EmptyHostnames)?,
                )?,
            ),
            message: input.message.try_into()?,
            listen: input.http.listen,
            source: crate::source::Settings::new(input.http.trusted_proxies)?,
            requests: crate::identity::Settings::new(input.http.origin, input.challenge.site_key)?,
            services: input.identity.parse(input.wechat.website.app_id.parse()?)?,
            process: crate::process::Settings::new(Duration::from_secs(
                input.process.stop_seconds,
            ))?,
        })
    }
}

impl TryFrom<Message> for message::settings::Settings {
    type Error = Error;

    fn try_from(input: Message) -> Result<Self, Error> {
        let signature = message::settings::Signature::new(input.signature)?;
        let template = input.template.try_into()?;
        Ok(Self::new(signature, template, input.timeout_milliseconds)?)
    }
}

impl TryFrom<Templates> for message::settings::Templates {
    type Error = Error;

    fn try_from(input: Templates) -> Result<Self, Error> {
        Ok(Self::new(
            message::settings::TemplateCode::new(input.authentication)?,
            message::settings::TemplateCode::new(input.mobile_change)?,
            message::settings::TemplateCode::new(input.mobile_binding)?,
        ))
    }
}

/// Loading and construction causes are retained without exposing submitted values.
#[derive(Error)]
pub enum Error {
    #[error("{0}")]
    Avatar(#[from] domain::identity::avatar::link::SettingsError),
    #[error("{0}")]
    AvatarRetention(#[from] domain::identity::avatar::retention::SettingsError),
    #[error("{0}")]
    Website(#[from] domain::identity::website::Error),
    #[error("{0}")]
    Session(#[from] authentication::session::settings::Error),
    #[error("{0}")]
    Source(#[from] crate::source::SettingsError),
    #[error("{0}")]
    Requests(#[from] crate::identity::settings::Error),
    #[error("{0}")]
    Capacity(#[from] domain::startup::CapacityError),
    #[error("{0}")]
    Limits(#[from] message::limits::Error),
    #[error("{0}")]
    Registration(#[from] domain::registration::LimitsError),
    #[error("{0}")]
    Process(#[from] crate::process::SettingsError),
    #[error("database.{0}")]
    Authority(#[from] crate::database::SettingsError),
    #[error("{0}")]
    Database(#[from] database::settings::Error),
    #[error("cannot load deployment settings")]
    Config(#[source] Box<config::ConfigError>),
    #[error("invalid deployment settings at {path}")]
    Input {
        path: serde_path_to_error::Path,
        #[source]
        source: Box<config::ConfigError>,
    },
    #[error("hostnames must not be empty")]
    EmptyHostnames,
    #[error("{0}")]
    Hostnames(#[from] challenge::settings::HostnamesError),
    #[error("{0}")]
    Signature(#[from] message::settings::SignatureError),
    #[error("{0}")]
    Template(#[from] message::settings::TemplateCodeError),
    #[error("{0}")]
    Timeout(#[from] message::settings::TimeoutError),
}

impl From<config::ConfigError> for Error {
    fn from(source: config::ConfigError) -> Self {
        Self::Config(Box::new(source))
    }
}

impl From<database::assembly::input::Error> for Error {
    fn from(source: database::assembly::input::Error) -> Self {
        match source {
            database::assembly::input::Error::Authority(error) => Self::Authority(error),
            database::assembly::input::Error::Database(error) => Self::Database(error),
        }
    }
}

impl From<serde_path_to_error::Error<config::ConfigError>> for Error {
    fn from(source: serde_path_to_error::Error<config::ConfigError>) -> Self {
        Self::Input {
            path: source.path().clone(),
            source: Box::new(source.into_inner()),
        }
    }
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{env, process::Command};

    use config::Environment;
    use domain::identity::{challenge, message};
    use nonempty::NonEmpty;

    use crate::settings::{Error, Input, Settings};

    fn input() -> Vec<(String, String)> {
        [
            ("NUCLEUS_CHALLENGE__SITE_KEY", "fixture-site-key"),
            (
                "NUCLEUS_CHALLENGE__HOSTNAMES",
                r#"["Example.com","127.0.0.1"]"#,
            ),
            ("NUCLEUS_MESSAGE__SIGNATURE", " Signature "),
            ("NUCLEUS_MESSAGE__TEMPLATE__AUTHENTICATION", "Auth"),
            ("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_CHANGE", "Change"),
            ("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_BINDING", "Binding"),
            ("NUCLEUS_DATABASE__HOST", "127.0.0.1"),
            ("NUCLEUS_DATABASE__AUTHORITY", "/fictional/authority.pem"),
            ("NUCLEUS_DATABASE__PORT", "15432"),
            ("NUCLEUS_DATABASE__NAME", "fictional_test"),
            ("NUCLEUS_DATABASE__USER", "clerk"),
            ("NUCLEUS_DATABASE__CAPACITY", "2"),
            ("NUCLEUS_HTTP__LISTEN", "127.0.0.1:3000"),
            ("NUCLEUS_HTTP__ORIGIN", "https://example.com"),
            ("NUCLEUS_HTTP__TRUSTED_PROXIES", r#"["127.0.0.1/32"]"#),
            ("NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY", "16"),
            (
                "NUCLEUS_IDENTITY__AVATAR__ORIGIN",
                "https://avatars.example.com",
            ),
            ("NUCLEUS_WECHAT__WEBSITE__APP_ID", "website-fixture"),
        ]
        .into_iter()
        .map(|(key, value)| (key.into(), value.into()))
        .collect()
    }

    fn load(input: Vec<(String, String)>) -> Result<Settings, Error> {
        Input::load(Environment::default().source(Some(input.into_iter().collect())))?.try_into()
    }

    fn rejected(input: Vec<(String, String)>) -> Error {
        match load(input) {
            Ok(_) => panic!("invalid input must fail"),
            Err(error) => error,
        }
    }

    #[test]
    fn loads_c1_defaults_and_rejects_invalid_supplied_limits() {
        let raw = Input::load(Environment::default().source(Some(input().into_iter().collect())))
            .expect("defaults");
        assert_eq!(raw.process.stop_seconds, 30);
        assert_eq!(raw.identity.message.lifetime_seconds, 300);
        assert_eq!(raw.identity.message.mobile.interval_seconds, 60);
        assert_eq!(raw.identity.registration.hourly, 5);
        assert_eq!(raw.identity.avatar.lifetime_seconds, 86400);
        assert_eq!(raw.identity.avatar.retention_seconds, 86400);
        assert!(Settings::try_from(raw).is_ok());
        let mut lower = input();
        lower.push(("NUCLEUS_IDENTITY__REGISTRATION__HOURLY".into(), "2".into()));
        let raw = Input::load(Environment::default().source(Some(lower.into_iter().collect())))
            .expect("lower registration quota input");
        assert_eq!(raw.identity.registration.hourly, 2);
        assert!(Settings::try_from(raw).is_ok());
        for (key, value) in [
            ("PROCESS__STOP_SECONDS", "0"),
            ("CHALLENGE__SITE_KEY", ""),
            ("CHALLENGE__SITE_KEY", "has space"),
            ("PROCESS__STOP_SECONDS", ""),
            ("IDENTITY__AUTHENTICATION__CAPACITY", "0"),
            ("IDENTITY__MESSAGE__MOBILE__INTERVAL_SECONDS", "0"),
            ("IDENTITY__MESSAGE__MOBILE__HOURLY", "999999"),
            ("IDENTITY__MESSAGE__SOURCE__MINUTE", "0"),
            ("IDENTITY__MESSAGE__LIFETIME_SECONDS", "0"),
            ("IDENTITY__MESSAGE__DAILY", ""),
            ("IDENTITY__REGISTRATION__HOURLY", "0"),
            ("IDENTITY__REGISTRATION__HOURLY", "6"),
            ("IDENTITY__REGISTRATION__HOURLY", ""),
            ("IDENTITY__AVATAR__LIFETIME_SECONDS", "59"),
            ("IDENTITY__AVATAR__LIFETIME_SECONDS", "86401"),
            ("IDENTITY__AVATAR__LIFETIME_SECONDS", ""),
            ("IDENTITY__AVATAR__RETENTION_SECONDS", "0"),
            ("IDENTITY__AVATAR__RETENTION_SECONDS", ""),
            (
                "IDENTITY__AVATAR__RETENTION_SECONDS",
                "18446744073709551615",
            ),
            ("IDENTITY__AVATAR__ORIGIN", "http://avatars.example.com"),
            (
                "IDENTITY__AVATAR__ORIGIN",
                "https://avatars.example.com/a/..",
            ),
            ("IDENTITY__AVATAR__ORIGIN", "https://@avatars.example.com"),
            ("IDENTITY__MESSAGE__UNKNOWN", "sensitive-marker"),
            ("HTTP__LISTEN", "sensitive-marker"),
            ("HTTP__ORIGIN", "http://example.com"),
            ("HTTP__TRUSTED_PROXIES", "[]"),
            ("HTTP__TRUSTED_PROXIES", r#"["0.0.0.0/0"]"#),
            ("HTTP__TRUSTED_PROXIES", "sensitive-marker"),
        ] {
            let key = format!("NUCLEUS_{key}");
            let mut values = input();
            values.retain(|(name, _)| name != &key);
            values.push((key, value.into()));
            let error = rejected(values);
            assert!(!format!("{error:?}").contains("sensitive-marker"));
        }
    }

    #[test]
    fn loads_database_defaults_and_partial_budgets() {
        let mut values = input();
        for (key, value) in &mut values {
            if key == "NUCLEUS_DATABASE__CAPACITY" {
                *value = "65".into();
            }
        }
        assert!(load(values).is_ok());
        let raw = Input::load(Environment::default().source(Some(input().into_iter().collect())))
            .expect("valid input");
        assert!(Settings::try_from(raw).is_ok());
        let mut values = input();
        values.push((
            "NUCLEUS_DATABASE__BUDGETS__CONNECT_SECONDS".into(),
            "7".into(),
        ));
        let raw = Input::load(Environment::default().source(Some(values.into_iter().collect())))
            .expect("partial budgets");
        assert!(Settings::try_from(raw).is_ok());
    }

    #[test]
    fn retains_numeric_field_context_and_accepts_longer_budgets() {
        for field in [
            "DATABASE__PORT",
            "DATABASE__CAPACITY",
            "DATABASE__BUDGETS__WAIT_SECONDS",
            "DATABASE__BUDGETS__CONNECT_SECONDS",
            "DATABASE__BUDGETS__RECYCLE_SECONDS",
            "IDENTITY__AUTHENTICATION__CAPACITY",
            "IDENTITY__MESSAGE__LIFETIME_SECONDS",
            "IDENTITY__MESSAGE__DAILY",
            "IDENTITY__MESSAGE__MOBILE__INTERVAL_SECONDS",
            "IDENTITY__MESSAGE__MOBILE__HOURLY",
            "IDENTITY__MESSAGE__MOBILE__DAILY",
            "IDENTITY__MESSAGE__SOURCE__MINUTE",
            "IDENTITY__MESSAGE__SOURCE__HOURLY",
            "MESSAGE__TIMEOUT_MILLISECONDS",
            "PROCESS__STOP_SECONDS",
        ] {
            let key = format!("NUCLEUS_{field}");
            let mut values = input();
            values.retain(|(name, _)| name != &key);
            values.push((key, "sensitive-marker".into()));
            let error = rejected(values);
            let expected = field.to_lowercase().replace("__", ".");
            assert!(format!("{error:?}").contains(&expected));
            assert!(!format!("{error:?}").contains("sensitive-marker"));
            assert!(std::error::Error::source(&error).is_some());
        }
        let mut values = input();
        values.push((
            "NUCLEUS_DATABASE__BUDGETS__CONNECT_SECONDS".into(),
            "61".into(),
        ));
        assert!(load(values).is_ok());
        let mut values = input();
        values.retain(|(key, _)| key != "NUCLEUS_DATABASE__CAPACITY");
        values.push(("NUCLEUS_DATABASE__CAPACITY".into(), usize::MAX.to_string()));
        assert!(matches!(
            rejected(values),
            Error::Database(platform::database::settings::Error::CapacityLimit)
        ));
        for seconds in [9223372036854775808_u64, u64::MAX] {
            let mut values = input();
            values.push((
                "NUCLEUS_DATABASE__BUDGETS__CONNECT_SECONDS".into(),
                seconds.to_string(),
            ));
            let raw =
                Input::load(Environment::default().source(Some(values.into_iter().collect())))
                    .expect("the complete u64 range parses before deadline validation");
            assert!(matches!(Settings::try_from(raw), Err(Error::Database(_))));
        }
    }

    #[test]
    fn rejects_invalid_database_inputs_without_exposing_values() {
        for (key, value, category) in [
            ("HOST", "sensitive host", "value"),
            ("NAME", "sensitive name", "value"),
            ("CAPACITY", "1", "value"),
            ("CAPACITY", "0", "value"),
            ("PORT", "0", "value"),
            ("PORT", "65536", "integer"),
            ("PORT", "", "integer"),
            ("CAPACITY", "2.0", "integer"),
            ("BUDGETS__WAIT_SECONDS", " 3", "integer"),
            ("BUDGETS__CONNECT_SECONDS", "true", "integer"),
            ("BUDGETS__RECYCLE_SECONDS", "", "integer"),
            ("BUDGETS__WAIT_SECONDS", "0", "value"),
            ("BUDGETS__CONNECT_SECONDS", "0", "value"),
            ("BUDGETS__RECYCLE_SECONDS", "0", "value"),
            ("BUDGETS__UNKNOWN", "sensitive-marker", "config"),
            ("PASSWORD", "sensitive-marker", "config"),
            ("USER", "", "value"),
        ] {
            let key = format!("NUCLEUS_DATABASE__{key}");
            let mut values = input();
            values.retain(|(name, _)| name != &key);
            values.push((key, value.into()));
            let error = rejected(values);
            assert!(!format!("{error:?} {error}").contains("sensitive"));
            match category {
                "value" => assert!(matches!(error, Error::Database(_))),
                _ => assert!(matches!(error, Error::Input { .. })),
            }
        }
    }

    #[test]
    fn loads_nested_settings_and_defaults_only_absent_timeout() {
        let raw = Input::load(Environment::default().source(Some(input().into_iter().collect())))
            .expect("valid input");
        assert_eq!(raw.message.signature, " Signature ");
        assert_eq!(raw.message.timeout_milliseconds, 10000);
        assert!(Settings::try_from(raw).is_ok());
        for timeout in ["1000", "10000"] {
            let mut values = input();
            values.push((
                "NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS".into(),
                timeout.into(),
            ));
            assert!(load(values).is_ok());
        }
        for timeout in [
            "",
            "true",
            "on",
            "1e3",
            "1000.0",
            " 1000",
            "1000 ",
            "１０００",
            "-0",
            "-1000",
            "18446744073709551616",
        ] {
            let mut values = input();
            values.push((
                "NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS".into(),
                timeout.into(),
            ));
            let error = rejected(values);
            assert!(matches!(error, Error::Input { .. }));
        }
        for timeout in ["999", "10001"] {
            let mut values = input();
            values.push((
                "NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS".into(),
                timeout.into(),
            ));
            assert!(matches!(rejected(values), Error::Timeout(_)));
        }
    }

    #[test]
    fn rejects_missing_fields_and_sections() {
        for index in 0..input().len() {
            let mut values = input();
            values.remove(index);
            let error = rejected(values);
            assert!(matches!(error, Error::Input { .. }));
        }
        let values = input()
            .into_iter()
            .filter(|(key, _)| !key.contains("TEMPLATE"))
            .collect();
        assert!(matches!(rejected(values), Error::Input { .. }));
    }

    #[test]
    fn rejects_unknown_fields_and_hides_submitted_values() {
        for key in [
            "NUCLEUS_UNKNOWN",
            "NUCLEUS_MESSAGE__UNKNOWN",
            "NUCLEUS_MESSAGE__TEMPLATE__UNKNOWN",
            "NUCLEUS_CHALLENGE__SECRET",
        ] {
            let mut values = input();
            values.push((key.into(), "sensitive-marker".into()));
            let error = rejected(values);
            assert!(matches!(&error, Error::Input { .. }));
            assert!(std::error::Error::source(&error).is_some());
            assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
        }
        for value in [
            "",
            "[]",
            "null",
            "{}",
            r#"["example.com",12345678]"#,
            r#"["example.com","sensitive marker"]"#,
        ] {
            let mut values = input();
            values.retain(|(key, _)| key != "NUCLEUS_CHALLENGE__HOSTNAMES");
            values.push(("NUCLEUS_CHALLENGE__HOSTNAMES".into(), value.into()));
            let error = rejected(values);
            assert!(!format!("{error:?} {error}").contains("sensitive"));
        }
    }

    #[test]
    fn format_errors_keep_paths_and_config_causes_without_exposing_values() {
        for (field, value) in [
            (
                "CHALLENGE__HOSTNAMES",
                r#"{"secret":"fictional-value-marker"}"#,
            ),
            ("CHALLENGE__HOSTNAMES", r#"["fictional-value-marker",123]"#),
            ("HTTP__TRUSTED_PROXIES", r#"["fictional-value-marker"]"#),
            (
                "HTTP__TRUSTED_PROXIES",
                r#"{"secret":"fictional-value-marker"}"#,
            ),
            ("HTTP__LISTEN", "fictional-value-marker"),
        ] {
            let key = format!("NUCLEUS_{field}");
            let mut values = input();
            values.retain(|(name, _)| name != &key);
            values.push((key, value.into()));
            let error = rejected(values);
            let path = field.to_lowercase().replace("__", ".");
            assert_eq!(
                format!("{error:?}"),
                format!("invalid deployment settings at {path}")
            );
            assert_eq!(
                format!("{error}"),
                format!("invalid deployment settings at {path}")
            );
            assert!(matches!(&error, Error::Input { .. }));
            assert!(
                std::error::Error::source(&error)
                    .expect("original configuration error")
                    .is::<Box<config::ConfigError>>()
            );
        }
        let mut values = input();
        values.retain(|(name, _)| name != "NUCLEUS_HTTP__TRUSTED_PROXIES");
        values.push((
            "NUCLEUS_HTTP__TRUSTED_PROXIES".into(),
            r#"["192.0.2.0/24"]"#.into(),
        ));
        let raw = Input::load(Environment::default().source(Some(values.into_iter().collect())))
            .expect("JSON array of CIDR strings");
        assert_eq!(
            raw.http.trusted_proxies,
            vec!["192.0.2.0/24".parse::<ipnet::IpNet>().expect("network")]
        );
        assert!(Settings::try_from(raw).is_ok());
    }

    #[test]
    fn conversion_retains_business_error_types() {
        for (index, (key, _)) in input()
            .iter()
            .enumerate()
            .filter(|(_, (key, _))| key.starts_with("NUCLEUS_MESSAGE__"))
        {
            let mut values = input();
            values[index].1.clear();
            let error = rejected(values);
            if key == "NUCLEUS_MESSAGE__SIGNATURE" {
                assert!(matches!(error, Error::Signature(_)));
            } else {
                assert!(matches!(error, Error::Template(_)));
            }
        }
        for (json, expected) in [
            ("{}", "json"),
            ("[]", "empty"),
            (r#"["-example.com"]"#, "host"),
        ] {
            let mut values = input();
            values.retain(|(key, _)| key != "NUCLEUS_CHALLENGE__HOSTNAMES");
            values.push(("NUCLEUS_CHALLENGE__HOSTNAMES".into(), json.into()));
            let error = rejected(values);
            match expected {
                "json" => assert!(matches!(error, Error::Input { .. })),
                "empty" => assert!(matches!(error, Error::EmptyHostnames)),
                _ => assert!(matches!(error, Error::Hostnames(_))),
            }
        }
    }

    #[test]
    fn diagnostics_preserve_safe_context_without_exposing_sources() {
        let source = match challenge::settings::AllowedHostnames::new(NonEmpty::from((
            "example.com".into(),
            vec!["sensitive host marker".into()],
        ))) {
            Ok(_) => panic!("invalid host must fail"),
            Err(source) => source,
        };
        let error = Error::from(source);
        let diagnostic = format!("{error:?} {error}");
        assert!(diagnostic.contains("hostname at index 1"));
        assert!(!diagnostic.contains("sensitive host marker"));
        assert!(std::error::Error::source(&error).is_some());

        let error = Error::from(message::settings::TimeoutError);
        assert!(format!("{error:?}").contains("1000 to 10000 milliseconds"));

        let error = Error::from(config::ConfigError::Message(
            "sensitive source marker".into(),
        ));
        let diagnostic = format!("{error:?} {error}");
        assert!(diagnostic.contains("cannot load deployment settings"));
        assert!(!diagnostic.contains("sensitive source marker"));
        assert!(std::error::Error::source(&error).is_some());
    }

    #[test]
    fn reads_exported_environment_without_mutating_the_parent() {
        if env::var_os("TEST_NUCLEUS_SETTINGS_CHILD").is_some() {
            assert!(Settings::load().is_ok());
            return;
        }
        let output = Command::new(env::current_exe().expect("test executable"))
            .args([
                "--exact",
                "settings::tests::reads_exported_environment_without_mutating_the_parent",
            ])
            .env_clear()
            .envs(input())
            .env("TEST_NUCLEUS_SETTINGS_CHILD", "1")
            .output()
            .expect("child starts");
        assert!(
            output.status.success(),
            "child failed: {}",
            String::from_utf8_lossy(&output.stdout)
        );
    }
}
