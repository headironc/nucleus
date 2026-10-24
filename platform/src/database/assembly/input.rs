//! Database input shape and controlled conversion; callers select and load sources.

use std::{fmt, time::Duration};

use serde::Deserialize;
use serde_with::{DisplayFromStr, serde_as};
use thiserror::Error;

use crate::database::{self, assembly};

#[serde_as]
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Input {
    user: String,
    authority: String,
    host: String,
    name: String,
    #[serde_as(as = "DisplayFromStr")]
    capacity: usize,
    #[serde_as(as = "DisplayFromStr")]
    port: u16,
    #[serde(default)]
    budgets: Budgets,
}

#[serde_as]
#[derive(Deserialize)]
#[serde(default, deny_unknown_fields)]
struct Budgets {
    #[serde_as(as = "DisplayFromStr")]
    wait_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    connect_seconds: u64,
    #[serde_as(as = "DisplayFromStr")]
    recycle_seconds: u64,
}

impl Default for Budgets {
    fn default() -> Self {
        Self {
            wait_seconds: database::settings::DEFAULT_WAIT_SECONDS,
            connect_seconds: database::settings::DEFAULT_CONNECT_SECONDS,
            recycle_seconds: database::settings::DEFAULT_RECYCLE_SECONDS,
        }
    }
}

impl TryFrom<Input> for assembly::Settings {
    type Error = Error;

    fn try_from(input: Input) -> Result<Self, Error> {
        let budgets = database::settings::Budgets::new(
            Duration::from_secs(input.budgets.wait_seconds),
            Duration::from_secs(input.budgets.connect_seconds),
            Duration::from_secs(input.budgets.recycle_seconds),
        )?;
        let connection = database::Settings::new(
            input.host,
            input.port,
            input.name,
            input.user,
            input.capacity,
            budgets,
        )?;
        Ok(Self::new(connection, input.authority)?)
    }
}

/// Construction causes retain their types without exposing submitted values.
#[derive(Error)]
pub enum Error {
    #[error("database.{0}")]
    Authority(#[from] assembly::SettingsError),
    #[error("{0}")]
    Database(#[from] database::settings::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use serde_json::{Value, json};

    use crate::database::{
        assembly::{Settings, input::Input},
        settings,
    };

    fn input() -> Value {
        json!({
            "user": "clerk",
            "authority": "/fictional/authority.pem",
            "host": "127.0.0.1",
            "name": "fictional_test",
            "capacity": "2",
            "port": "15432",
        })
    }

    #[test]
    fn loads_defaults_and_partial_budgets() {
        let raw: Input = serde_json::from_value(input()).expect("valid input");
        assert_eq!(raw.port, 15432);
        assert_eq!(raw.budgets.wait_seconds, 3);
        assert_eq!(raw.budgets.connect_seconds, 5);
        assert_eq!(raw.budgets.recycle_seconds, 3);
        assert!(Settings::try_from(raw).is_ok());
        let mut input = input();
        input["budgets"] = json!({"connect_seconds": "7"});
        let raw: Input = serde_json::from_value(input).expect("partial budgets");
        assert_eq!(raw.budgets.wait_seconds, 3);
        assert_eq!(raw.budgets.connect_seconds, 7);
        assert_eq!(raw.budgets.recycle_seconds, 3);
        assert!(Settings::try_from(raw).is_ok());
    }

    #[test]
    fn parses_complete_u64_range_before_deadline_validation() {
        for seconds in [9223372036854775808_u64, u64::MAX] {
            let mut input = input();
            input["budgets"] = json!({"connect_seconds": seconds.to_string()});
            let raw: Input = serde_json::from_value(input).expect("u64 budget input");
            assert_eq!(raw.budgets.connect_seconds, seconds);
            assert!(matches!(
                Settings::try_from(raw),
                Err(super::Error::Database(settings::Error::Budget(
                    settings::Stage::Connect
                )))
            ));
        }
    }
}
