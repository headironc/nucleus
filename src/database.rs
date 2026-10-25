//! Application database file loading and connection assembly.

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
    let directory = credentials::directory()?;
    let input: Input = credentials::read(&directory, Filename::Database)?;
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
    use std::{env, fs, path::Path, process::Command, time::Duration};

    use platform::database::{
        self, assembly,
        credentials::{Input, Password},
    };
    use rcgen::{CertifiedKey, generate_simple_self_signed};
    use rustls::RootCertStore;
    use tempfile::TempDir;
    use tokio::{runtime::Runtime, time::timeout};

    use crate::{
        credentials::{self, Filename},
        database::{Error, Settings, load},
    };

    const PASSWORD: &str = "fictional database password";
    const JSON: &str = r#"{"version":1,"password":"fictional database password"}"#;

    fn connection(port: u16) -> database::Settings {
        database::Settings::new(
            "127.0.0.1".into(),
            port,
            "postgres".into(),
            "clerk".into(),
            2,
            database::settings::Budgets::default(),
        )
        .expect("fictional settings")
    }

    fn certificate(directory: &Path) -> CertifiedKey<rcgen::KeyPair> {
        let certificate =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).expect("fictional certificate");
        fs::write(directory.join("authority.pem"), certificate.cert.pem())
            .expect("write authority");
        certificate
    }

    fn child(directory: &Path, port: u16, case: &str) {
        let mut command = Command::new(env::current_exe().expect("test executable"));
        command
            .args(["--exact", "database::tests::loads_only_declared_sources"])
            .env_clear()
            .current_dir(directory)
            .env("TEST_NUCLEUS_DATABASE_CHILD", case)
            .env("CREDENTIALS_DIRECTORY", directory)
            .env("NUCLEUS_DATABASE__HOST", "127.0.0.1")
            .env("NUCLEUS_DATABASE__PORT", port.to_string())
            .env("NUCLEUS_DATABASE__NAME", "postgres")
            .env("NUCLEUS_DATABASE__USER", "clerk")
            .env("NUCLEUS_DATABASE__CAPACITY", "2")
            .env("NUCLEUS_HTTP__LISTEN", "127.0.0.1:3000")
            .env("NUCLEUS_HTTP__ORIGIN", "https://example.com")
            .env("NUCLEUS_HTTP__TRUSTED_PROXIES", r#"["127.0.0.1/32"]"#)
            .env("NUCLEUS_IDENTITY__AUTHENTICATION__CAPACITY", "16")
            .env("NUCLEUS_IDENTITY__AVATAR__ORIGIN", "https://example.com")
            .env("NUCLEUS_STORAGE__ROOT", "/fictional/avatar-root")
            .env("NUCLEUS_STORAGE__MOUNT", "/")
            .env("NUCLEUS_STORAGE__DEVICE", "1")
            .env("NUCLEUS_WECHAT__WEBSITE__APP_ID", "website-fixture")
            .env(
                "NUCLEUS_DATABASE__AUTHORITY",
                directory.join("authority.pem"),
            )
            .env("NUCLEUS_CHALLENGE__HOSTNAMES", r#"["example.com"]"#)
            .env("NUCLEUS_CHALLENGE__SITE_KEY", "fixture-site-key")
            .env("NUCLEUS_MESSAGE__SIGNATURE", "Test")
            .env("NUCLEUS_MESSAGE__TEMPLATE__AUTHENTICATION", "Auth")
            .env("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_CHANGE", "Change")
            .env("NUCLEUS_MESSAGE__TEMPLATE__MOBILE_BINDING", "Binding")
            .env("PGPASSWORD", "ignored-password");
        match case {
            "missing_directory" => {
                command.env_remove("CREDENTIALS_DIRECTORY");
            }
            "empty_directory" => {
                command.env("CREDENTIALS_DIRECTORY", "");
            }
            "relative_directory" => {
                command.env("CREDENTIALS_DIRECTORY", ".");
            }
            _ => {}
        }
        let output = command.output().expect("isolated child starts");
        assert!(
            output.status.success(),
            "child {case} failed: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn validates_authority_location_without_reading_files() {
        for path in ["", "authority.pem", "/fictional/authority\n.pem"] {
            assert!(Settings::new(connection(1), path.into()).is_err());
        }
        assert!(Settings::new(connection(1), "/fictional/missing.pem".into()).is_ok());
    }

    #[test]
    fn enforces_database_json_and_password_boundaries() {
        let directory = TempDir::new().expect("fixture directory");
        let cert = certificate(directory.path());
        for bytes in [
            b"".as_slice(),
            b"[]",
            b"null",
            &[0xff],
            br#"[1,"secret"]"#,
            br#"{"version":1}"#,
            br#"{"password":"secret"}"#,
            br#"{"version":2,"password":"secret"}"#,
            br#"{"version":1.0,"password":"secret"}"#,
            br#"{"version":1,"version":1,"password":"secret"}"#,
            br#"{"version":1,"password":"secret","password":"other"}"#,
            br#"{"version":1,"password":"secret","extra":true}"#,
            br#"{"version":1,"password":123}"#,
            br#"{"version":1,"password":null}"#,
            br#"{"version":1,"password":true}"#,
            br#"{"version":1,"password":[]}"#,
            br#"{"version":1,"password":{}}"#,
            br#"{"version":1,"password":"secret"} {}"#,
        ] {
            fs::write(directory.path().join("database.json"), bytes)
                .expect("write invalid fixture");
            let result = credentials::read::<Input>(directory.path(), Filename::Database);
            assert!(matches!(result, Err(credentials::Error::Decode { .. })));
        }
        for password in [
            String::new(),
            "secret\n".into(),
            "secret\0".into(),
            "x".repeat(4097),
        ] {
            let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"password":password}))
                .expect("fixture JSON");
            fs::write(directory.path().join("database.json"), bytes).expect("write fixture");
            let input: Input =
                credentials::read(directory.path(), Filename::Database).expect("valid JSON shape");
            let error = Password::try_from(input).expect_err("invalid password value");
            assert_eq!(
                format!("{error:?} {error}"),
                "Error invalid database password"
            );
        }
        for password in [" leading and trailing spaces ".to_owned(), "é".repeat(2048)] {
            let bytes = serde_json::to_vec(&serde_json::json!({"version":1,"password":password}))
                .expect("fixture JSON");
            fs::write(directory.path().join("database.json"), bytes).expect("write fixture");
            let input: Input =
                credentials::read(directory.path(), Filename::Database).expect("valid credential");
            assert!(
                database::Connections::new(
                    connection(1),
                    Password::try_from(input).expect("valid password"),
                    assembly::roots(cert.cert.pem().as_bytes()).expect("fixture trust anchor")
                )
                .is_ok()
            );
        }
    }

    #[test]
    fn diagnostics_hide_sources_and_paths() {
        let errors = [
            Error::AuthorityRead(std::io::Error::other("sensitive path marker")),
            Error::Credentials(credentials::Error::Decode {
                file: Filename::Database,
                source: <serde_json::Error as serde::de::Error>::custom(
                    "sensitive password marker",
                ),
            }),
        ];
        for error in errors {
            assert!(!format!("{error:?} {error}").contains("sensitive"));
            assert!(std::error::Error::source(&error).is_some());
        }
    }

    #[test]
    fn loads_only_declared_sources() {
        if let Ok(case) = env::var("TEST_NUCLEUS_DATABASE_CHILD") {
            let settings = crate::settings::Settings::load().expect("exported fictional settings");
            let result = load(settings.database);
            match case.as_str() {
                "valid" => {
                    assert!(result.is_ok());
                }
                "missing_directory" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::DirectoryMissing))
                )),
                "empty_directory" | "relative_directory" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::DirectoryInvalid))
                )),
                "missing_file" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::Read {
                        file: Filename::Database,
                        ..
                    }))
                )),
                "missing_authority" => assert!(matches!(result, Err(Error::AuthorityRead(_)))),
                "empty_authority" => assert!(matches!(
                    result,
                    Err(Error::Assembly(assembly::Error::Build(
                        database::BuildError::EmptyAuthority
                    )))
                )),
                "invalid_pem" => assert!(matches!(
                    result,
                    Err(Error::Assembly(assembly::Error::Pem(_)))
                )),
                "invalid_der" => assert!(matches!(
                    result,
                    Err(Error::Assembly(assembly::Error::Certificate(_)))
                )),
                "connect" | "wrong_password" | "wrong_authority" => {
                    let connections = result.expect("assemble test connections");
                    Runtime::new().expect("test runtime").block_on(async {
                        let online = timeout(Duration::from_secs(10), connections.acquire()).await.expect("bounded connect");
                        if case != "connect" { assert!(online.is_err()); return; }
                        let online = online.expect("online connection from loaded files");
                        let row = online.client().query_one("SELECT current_user, ssl FROM pg_stat_ssl WHERE pid = pg_backend_pid()", &[]).await.expect("identity and TLS");
                        assert_eq!(row.get::<_, String>(0), "clerk");
                        assert!(row.get::<_, bool>(1));
                        let recovery = connections.recover().await.expect("recovery from loaded files");
                        assert_eq!(recovery.client().query_one("SELECT current_user", &[]).await.expect("recovery identity").get::<_, String>(0), "clerk");
                        recovery.terminate().await.expect("end recovery driver");
                        database::Connections::discard(online).await.expect("end online driver");
                    });
                }
                _ => panic!("unknown child case"),
            }
            return;
        }
        let directory = TempDir::new().expect("fixture directory");
        let cert = certificate(directory.path());
        fs::write(directory.path().join("database.json"), JSON).expect("fixture credential");
        fs::write(
            directory.path().join(".env"),
            "CREDENTIALS_DIRECTORY=/not-loaded\n",
        )
        .expect("fictional ignored env file");
        for case in [
            "valid",
            "missing_directory",
            "empty_directory",
            "relative_directory",
        ] {
            child(directory.path(), 1, case);
        }
        fs::remove_file(directory.path().join("database.json")).expect("remove test credential");
        fs::write(directory.path().join("database-password"), PASSWORD)
            .expect("legacy name is not a source");
        child(directory.path(), 1, "missing_file");
        fs::write(directory.path().join("database.json"), JSON).expect("restore fixture");
        fs::remove_file(directory.path().join("authority.pem")).expect("remove authority");
        child(directory.path(), 1, "missing_authority");
        for (case, pem) in [
            ("empty_authority", ""),
            (
                "invalid_pem",
                "-----BEGIN CERTIFICATE-----\n!\n-----END CERTIFICATE-----\n",
            ),
            (
                "invalid_der",
                "-----BEGIN CERTIFICATE-----\nAQID\n-----END CERTIFICATE-----\n",
            ),
        ] {
            fs::write(directory.path().join("authority.pem"), pem)
                .expect("invalid authority fixture");
            child(directory.path(), 1, case);
        }
        // Bundles are supported; no connection is made during assembly.
        fs::write(
            directory.path().join("authority.pem"),
            format!("{}{}", cert.cert.pem(), cert.cert.pem()),
        )
        .expect("certificate bundle");
        child(directory.path(), 1, "valid");
    }

    #[test]
    fn connects_to_isolated_postgres_using_loaded_files() {
        let directory = TempDir::new().expect("isolated server directory");
        let path = directory.path();
        let server = fixture::Server::start();
        fs::copy(server.authority(), path.join("authority.pem")).expect("test authority");
        fs::write(path.join("database.json"), JSON).expect("fictional credential");
        let port = server.port();
        Runtime::new().expect("fixture runtime").block_on(async {
            let settings = database::Settings::new(
                "127.0.0.1".into(),
                port,
                "postgres".into(),
                "postgres".into(),
                2,
                database::settings::Budgets::default(),
            )
            .expect("fixture admin settings");
            let mut roots = RootCertStore::empty();
            roots
                .add(server.certificate().clone())
                .expect("fixture trust anchor");
            let connections = database::Connections::new(
                settings,
                Password::new(fixture::PASSWORD.into()).expect("valid fixture password"),
                roots,
            )
            .expect("fixture admin connections");
            let admin = connections.recover().await.expect("fixture admin login");
            admin
                .client()
                .batch_execute("CREATE ROLE clerk LOGIN PASSWORD 'fictional database password'")
                .await
                .expect("fixture application role");
            admin.terminate().await.expect("end fixture admin");
        });
        child(path, port, "connect");
        fs::write(
            path.join("database.json"),
            r#"{"version":1,"password":"wrong fictional password"}"#,
        )
        .expect("wrong password");
        child(path, port, "wrong_password");
        fs::write(path.join("database.json"), JSON).expect("restore password");
        let unrelated =
            generate_simple_self_signed(vec!["127.0.0.1".into()]).expect("unrelated CA");
        fs::write(path.join("authority.pem"), unrelated.cert.pem()).expect("wrong authority");
        child(path, port, "wrong_authority");
    }
}
