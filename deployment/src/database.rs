//! Deployment database file loading and connection assembly.

use std::{fmt, fs, io};

pub use platform::database::assembly::{Settings, SettingsError};
use platform::database::{
    self, assembly,
    credentials::{Input, Password},
};
use thiserror::Error;

use crate::credentials::{self, Filename};

/// Load only declared sources and assemble without opening database connections.
pub fn load(settings: Settings) -> Result<database::Connections, Error> {
    load_file(settings, Filename::Database)
}

/// Administration uses the application's credentials and permission boundary.
pub(crate) fn runtime(settings: Settings) -> Result<database::Connections, Error> {
    load_file(settings, Filename::RuntimeDatabase)
}

fn load_file(settings: Settings, filename: Filename) -> Result<database::Connections, Error> {
    let directory = credentials::directory()?;
    let input: Input = credentials::read(&directory, filename)?;
    let password: Password = input.try_into()?;
    let bytes = fs::read(settings.authority()).map_err(Error::AuthorityRead)?;
    Ok(settings.build(password, assembly::roots(&bytes)?)?)
}

/// Safe diagnostics preserve original causes without paths or credential values.
#[derive(Error)]
pub enum Error {
    #[error("{0}")]
    Password(#[from] database::credentials::Error),
    #[error("{0}")]
    Credentials(#[from] credentials::Error),
    #[error("cannot read database authority file")]
    AuthorityRead(#[source] io::Error),
    #[error("{0}")]
    Assembly(#[from] assembly::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{env, fs, process::Command};

    use tempfile::TempDir;

    use crate::{credentials, database, settings::Settings};

    #[test]
    fn validates_authority_location_without_reading_files() {
        for path in [
            "",
            "authority.pem",
            "/fictional/authority\n.pem",
            "/fictional/authority.pem",
        ] {
            let connection = platform::database::Settings::new(
                "localhost".into(),
                5432,
                "fixture".into(),
                "steward".into(),
                2,
                platform::database::settings::Budgets::default(),
            )
            .expect("fictional connection");
            assert_eq!(
                database::Settings::new(connection, path.into()).is_ok(),
                path == "/fictional/authority.pem"
            );
        }
    }

    #[test]
    fn loads_only_the_fixed_database_credential() {
        if let Ok(case) = env::var("TEST_DEPLOYMENT_CREDENTIALS") {
            let result = database::load(Settings::load().expect("settings").into_database());
            if case == "valid" {
                assert!(result.is_ok());
            } else if case == "missing_directory" {
                assert!(matches!(
                    result,
                    Err(database::Error::Credentials(
                        credentials::Error::DirectoryMissing
                    ))
                ));
            } else if case == "relative_directory" {
                assert!(matches!(
                    result,
                    Err(database::Error::Credentials(
                        credentials::Error::DirectoryInvalid
                    ))
                ));
            } else {
                let error = result.err().expect("invalid input");
                assert!(!format!("{error:?}").contains("sensitive"));
            }
            return;
        }
        let directory = TempDir::new().expect("directory");
        let path = directory.path();
        let certificate =
            rcgen::generate_simple_self_signed(vec!["localhost".into()]).expect("certificate");
        fs::write(path.join("authority.pem"), certificate.cert.pem()).expect("certificate file");
        fs::write(path.join(".env"), "NUCLEUS_MIGRATION__PORT=invalid\n").expect("ignored env");
        let run = |case: &str| {
            let mut command = Command::new(env::current_exe().expect("executable"));
            command
                .args([
                    "--exact",
                    "database::tests::loads_only_the_fixed_database_credential",
                ])
                .env_clear()
                .current_dir(path)
                .env("TEST_DEPLOYMENT_CREDENTIALS", case)
                .env("CREDENTIALS_DIRECTORY", path)
                .env("NUCLEUS_MIGRATION__HOST", "localhost")
                .env("NUCLEUS_MIGRATION__PORT", "5432")
                .env("NUCLEUS_MIGRATION__NAME", "fixture")
                .env("NUCLEUS_MIGRATION__AUTHORITY", path.join("authority.pem"))
                .env("NUCLEUS_MIGRATION__USER", "steward")
                .env("NUCLEUS_MIGRATION__RUNTIME_USER", "clerk");
            match case {
                "missing_directory" => {
                    command.env_remove("CREDENTIALS_DIRECTORY");
                }
                "relative_directory" => {
                    command.env("CREDENTIALS_DIRECTORY", ".");
                }
                _ => {}
            }
            let result = command.output().expect("isolated child");
            assert!(
                result.status.success(),
                "{case}: {}",
                String::from_utf8_lossy(&result.stderr)
            );
        };
        run("missing");
        run("missing_directory");
        run("relative_directory");
        fs::write(
            path.join("migration-password"),
            r#"{"version":1,"password":"fictional-password"}"#,
        )
        .expect("password");
        run("valid");
        for invalid in [
            r#"[1,"fictional-password"]"#,
            r#"{"version":2,"password":"fictional-password"}"#,
            r#"{"version":1,"password":""}"#,
            r#"{"version":1,"password":"fictional-password","sensitive":true}"#,
        ] {
            fs::write(path.join("migration-password"), invalid).expect("fixture");
            run("invalid");
        }
    }
}
