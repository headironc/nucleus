//! Deployment-only environment inputs. Application settings are not read.

use std::{fmt, num::ParseIntError};

use config::{Config, Environment};
use platform::database::{User, settings::Budgets, user};
use serde::Deserialize;
use thiserror::Error;

use crate::database;

pub struct Settings {
    runtime_user: User,
    database: database::Settings,
}

impl Settings {
    fn new(runtime_user: User, database: database::Settings) -> Self {
        Self {
            runtime_user,
            database,
        }
    }

    pub fn runtime_user(&self) -> &User {
        &self.runtime_user
    }

    pub fn into_database(self) -> database::Settings {
        self.database
    }

    pub fn load() -> Result<Self, Error> {
        Input::load(Environment::default())?.try_into()
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    migration: Migration,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Migration {
    user: String,
    runtime_user: String,
    host: String,
    port: String,
    name: String,
    authority: String,
}

impl Input {
    fn load(source: Environment) -> Result<Self, Error> {
        Ok(Config::builder()
            .add_source(
                source
                    .prefix("NUCLEUS")
                    .prefix_separator("_")
                    .separator("__")
                    .try_parsing(false)
                    .ignore_empty(false),
            )
            .build()?
            .try_deserialize()?)
    }
}

impl TryFrom<Input> for Settings {
    type Error = Error;

    fn try_from(input: Input) -> Result<Self, Error> {
        input.migration.try_into()
    }
}

impl TryFrom<Migration> for Settings {
    type Error = Error;

    fn try_from(input: Migration) -> Result<Self, Error> {
        let connection = platform::database::Settings::new(
            input.host,
            input.port.parse()?,
            input.name,
            input.user,
            2,
            Budgets::default(),
        )?;
        Ok(Self::new(
            User::new(input.runtime_user)?,
            database::Settings::new(connection, input.authority)?,
        ))
    }
}

#[derive(Error)]
pub enum Error {
    #[error("invalid runtime database user")]
    RuntimeUser(#[from] user::Error),
    #[error("cannot load migration settings")]
    Config(#[source] Box<config::ConfigError>),
    #[error("invalid migration port")]
    Port(#[from] ParseIntError),
    #[error("migration.{0}")]
    Database(#[from] database::SettingsError),
    #[error("{0}")]
    Connection(#[from] platform::database::settings::Error),
}

impl From<config::ConfigError> for Error {
    fn from(error: config::ConfigError) -> Self {
        Self::Config(Box::new(error))
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
    use std::collections::HashMap;

    use config::Environment;

    use crate::settings::{Input, Settings};

    #[test]
    fn accepts_only_complete_migration_inputs_and_rejects_invalid_values() {
        let values = HashMap::from([
            ("NUCLEUS_MIGRATION__USER".into(), "steward".into()),
            ("NUCLEUS_MIGRATION__RUNTIME_USER".into(), "clerk".into()),
            ("NUCLEUS_MIGRATION__HOST".into(), "localhost".into()),
            ("NUCLEUS_MIGRATION__PORT".into(), "5432".into()),
            ("NUCLEUS_MIGRATION__NAME".into(), "fixture".into()),
            (
                "NUCLEUS_MIGRATION__AUTHORITY".into(),
                "/fictional/authority.pem".into(),
            ),
        ]);
        let input = Input::load(Environment::default().source(Some(values.clone())))
            .expect("complete input");
        assert!(Settings::try_from(input).is_ok());
        for name in values.keys() {
            let mut missing = values.clone();
            missing.remove(name);
            assert!(Input::load(Environment::default().source(Some(missing))).is_err());
            let mut empty = values.clone();
            empty.insert(name.clone(), String::new());
            let input =
                Input::load(Environment::default().source(Some(empty))).expect("raw empty input");
            assert!(Settings::try_from(input).is_err());
        }
        let mut unknown = values.clone();
        unknown.insert("NUCLEUS_MIGRATION__UNKNOWN".into(), "postgres".into());
        assert!(Input::load(Environment::default().source(Some(unknown))).is_err());
        let mut unknown = values;
        unknown.insert("NUCLEUS_DATABASE__HOST".into(), "localhost".into());
        assert!(Input::load(Environment::default().source(Some(unknown))).is_err());
    }
}
