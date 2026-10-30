//! Administration selects runtime sources without requiring HTTP or supplier settings.

use std::{env, fmt};

use config::{Config, Environment};
use domain::identity::website;
use platform::database::assembly;

pub(super) struct Settings {
    pub(super) database: assembly::Settings,
    pub(super) website: website::AppId,
}

#[cfg(test)]
mod tests {
    #![allow(clippy::expect_used)]

    use std::{env, fs, path::Path, process::Command};

    use tempfile::TempDir;

    use crate::{
        admin::settings::{Error, Settings},
        credentials, database,
    };

    fn child(directory: &Path, test: &str) -> Command {
        let module = module_path!().split_once("::").expect("qualified module").1;
        let mut command = Command::new(env::current_exe().expect("test executable"));
        command
            .args(["--exact", &format!("{module}::{test}")])
            .env_clear()
            .current_dir(directory)
            .env("NUCLEUS_DATABASE__HOST", "localhost")
            .env("NUCLEUS_DATABASE__PORT", "5432")
            .env("NUCLEUS_DATABASE__NAME", "fictional_admin")
            .env("NUCLEUS_DATABASE__USER", "clerk")
            .env("NUCLEUS_DATABASE__CAPACITY", "2")
            .env(
                "NUCLEUS_DATABASE__AUTHORITY",
                directory.join("authority.pem"),
            )
            .env(
                "NUCLEUS_WECHAT__WEBSITE__APP_ID",
                "configured-website-fixture",
            );
        command
    }

    fn succeeds(command: &mut Command, case: &str) {
        let output = command.output().expect("isolated child");
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            output.status.success(),
            "{case}: stdout={} stderr={}",
            stdout,
            String::from_utf8_lossy(&output.stderr),
        );
        assert!(
            stdout.contains("test result: ok. 1 passed;"),
            "{case}: the exact child test must execute: {stdout}",
        );
    }

    #[test]
    fn loads_runtime_environment_without_other_capabilities() {
        if let Ok(case) = env::var("TEST_ADMIN_SETTINGS") {
            let result = Settings::load();
            if case == "valid" {
                let settings = result.expect("runtime settings suffice");
                assert_eq!(settings.website.as_ref(), "configured-website-fixture");
                assert_eq!(
                    settings.database.authority(),
                    Path::new(&env::var("NUCLEUS_DATABASE__AUTHORITY").expect("authority input")),
                );
            } else {
                let error = result.err().expect("invalid settings must fail");
                assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
                assert!(std::error::Error::source(&error).is_some());
                match case.as_str() {
                    "input" => assert!(matches!(error, Error::Input(_))),
                    "database" => assert!(matches!(error, Error::Database(_))),
                    "website_missing" => assert!(matches!(error, Error::WebsiteInput(_))),
                    "website_invalid" => assert!(matches!(error, Error::Website(_))),
                    _ => panic!("unknown child case"),
                }
            }
            return;
        }

        let directory = TempDir::new().expect("isolated environment directory");
        fs::write(
            directory.path().join(".env"),
            "NUCLEUS_DATABASE__HOST=localhost\nNUCLEUS_DATABASE__PORT=5432\nNUCLEUS_DATABASE__NAME=fictional_admin\nNUCLEUS_DATABASE__USER=clerk\nNUCLEUS_DATABASE__CAPACITY=2\nNUCLEUS_DATABASE__AUTHORITY=/fictional/authority.pem\nNUCLEUS_WECHAT__WEBSITE__APP_ID=dotenv-website\n",
        )
        .expect("ignored dotenv fixture");
        let run = |case: &str, key: Option<&str>, value: Option<&str>| {
            let mut command = child(
                directory.path(),
                "loads_runtime_environment_without_other_capabilities",
            );
            command
                .env("TEST_ADMIN_SETTINGS", case)
                .env("NUCLEUS_CHALLENGE__HOSTNAMES", "sensitive-marker")
                .env("NUCLEUS_MESSAGE__TIMEOUT_MILLISECONDS", "sensitive-marker")
                .env("NUCLEUS_HTTP__LISTEN", "sensitive-marker")
                .env(
                    "NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY",
                    "sensitive-marker",
                )
                .env("NUCLEUS_MIGRATION__HOST", "localhost")
                .env("NUCLEUS_MIGRATION__PORT", "5432")
                .env("NUCLEUS_MIGRATION__NAME", "fictional_migration")
                .env("NUCLEUS_MIGRATION__USER", "steward")
                .env("NUCLEUS_MIGRATION__RUNTIME_USER", "clerk")
                .env("NUCLEUS_MIGRATION__AUTHORITY", "/fictional/migration.pem");
            if let Some(key) = key {
                match value {
                    Some(value) => {
                        command.env(key, value);
                    }
                    None => {
                        command.env_remove(key);
                    }
                }
            }
            succeeds(&mut command, &format!("{case} {key:?} {value:?}"));
        };
        run("valid", None, None);
        run(
            "valid",
            Some("NUCLEUS_DATABASE__BUDGETS__CONNECT_SECONDS"),
            Some("61"),
        );
        for field in ["HOST", "PORT", "NAME", "USER", "CAPACITY", "AUTHORITY"] {
            run("input", Some(&format!("NUCLEUS_DATABASE__{field}")), None);
        }
        for (field, value, category) in [
            ("PORT", "", "input"),
            ("PORT", "65536", "input"),
            ("PORT", "５４３２", "input"),
            ("PORT", " 5432", "input"),
            ("CAPACITY", "2.0", "input"),
            ("CAPACITY", "true", "input"),
            ("CAPACITY", "-2", "input"),
            ("CAPACITY", "", "input"),
            ("BUDGETS__WAIT_SECONDS", " 3", "input"),
            ("BUDGETS__CONNECT_SECONDS", "true", "input"),
            ("BUDGETS__RECYCLE_SECONDS", "", "input"),
            ("PASSWORD", "sensitive-marker", "input"),
            ("BUDGETS__UNKNOWN", "sensitive-marker", "input"),
            ("HOST", "sensitive-marker host", "database"),
            ("HOST", "", "database"),
            ("NAME", "", "database"),
            ("USER", "", "database"),
            ("AUTHORITY", "", "database"),
            ("PORT", "0", "database"),
            ("CAPACITY", "1", "database"),
            ("BUDGETS__WAIT_SECONDS", "0", "database"),
            ("BUDGETS__CONNECT_SECONDS", "0", "database"),
            ("BUDGETS__RECYCLE_SECONDS", "0", "database"),
        ] {
            run(
                category,
                Some(&format!("NUCLEUS_DATABASE__{field}")),
                Some(value),
            );
        }
        run(
            "website_missing",
            Some("NUCLEUS_WECHAT__WEBSITE__APP_ID"),
            None,
        );
        for value in ["", "sensitive-marker website"] {
            run(
                "website_invalid",
                Some("NUCLEUS_WECHAT__WEBSITE__APP_ID"),
                Some(value),
            );
        }
    }

    #[test]
    fn loads_only_runtime_database_credentials() {
        if let Ok(case) = env::var("TEST_ADMIN_CREDENTIALS") {
            let result = database::runtime(Settings::load().expect("runtime settings").database);
            if case == "valid" {
                assert!(result.is_ok(), "runtime credentials assemble connections");
            } else {
                let error = result.err().expect("invalid runtime credential must fail");
                assert!(!format!("{error:?} {error}").contains("sensitive-marker"));
                match case.as_str() {
                    "missing" => assert!(matches!(
                        error,
                        database::Error::Credentials(credentials::Error::Read {
                            file: credentials::Filename::RuntimeDatabase,
                            source,
                        }) if source.kind() == std::io::ErrorKind::NotFound
                    )),
                    "invalid" => assert!(matches!(
                        error,
                        database::Error::Credentials(credentials::Error::Decode {
                            file: credentials::Filename::RuntimeDatabase,
                            ..
                        })
                    )),
                    "password" => assert!(matches!(error, database::Error::Password(_))),
                    _ => panic!("unknown child case"),
                }
            }
            return;
        }

        let directory = TempDir::new().expect("isolated credentials directory");
        let path = directory.path();
        let certificate = rcgen::generate_simple_self_signed(vec!["localhost".into()])
            .expect("local certificate");
        fs::write(path.join("authority.pem"), certificate.cert.pem()).expect("local trust roots");
        let run = |case: &str| {
            let mut command = child(path, "loads_only_runtime_database_credentials");
            command
                .env("TEST_ADMIN_CREDENTIALS", case)
                .env("CREDENTIALS_DIRECTORY", path);
            succeeds(&mut command, case);
        };
        let credential = r#"{"version":1,"password":"fictional-runtime-password"}"#;
        fs::write(path.join("database.json"), credential).expect("runtime credential");
        run("valid");
        fs::write(
            path.join("migration-password"),
            "sensitive-marker invalid JSON",
        )
        .expect("unused migration credential");
        run("valid");
        fs::write(path.join("migration-password"), credential).expect("migration credential");
        fs::remove_file(path.join("database.json")).expect("remove runtime credential");
        run("missing");
        fs::write(path.join("database.json"), "sensitive-marker invalid JSON")
            .expect("malformed runtime credential");
        run("invalid");
        fs::write(path.join("database.json"), r#"{"version":1,"password":""}"#)
            .expect("invalid runtime password");
        run("password");
    }
}

impl Settings {
    pub(super) fn load() -> Result<Self, Error> {
        let source = Environment::with_prefix("NUCLEUS_DATABASE")
            .prefix_separator("__")
            .separator("__")
            .try_parsing(false)
            .ignore_empty(false);
        let input: assembly::input::Input = Config::builder()
            .add_source(source)
            .build()?
            .try_deserialize()?;
        let website = env::var("NUCLEUS_WECHAT__WEBSITE__APP_ID").map_err(Error::WebsiteInput)?;
        Ok(Self {
            database: input.try_into()?,
            website: website.parse()?,
        })
    }
}

#[derive(thiserror::Error)]
pub enum Error {
    #[error("invalid NUCLEUS_DATABASE settings")]
    Input(#[from] config::ConfigError),
    #[error("{0}")]
    Database(#[from] assembly::input::Error),
    #[error("NUCLEUS_WECHAT__WEBSITE__APP_ID must be supplied as Unicode")]
    WebsiteInput(#[source] env::VarError),
    #[error("invalid NUCLEUS_WECHAT__WEBSITE__APP_ID: {0}")]
    Website(#[from] website::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, f)
    }
}
