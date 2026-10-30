//! Deployment key file decoding and validated metadata assembly.

use std::fmt;

use domain::keys::{self, input};
use thiserror::Error;

use crate::credentials::{self, Filename};

/// Derive metadata from the same validated root and use types the application loads.
pub fn load() -> Result<keys::Keys, Error> {
    let directory = credentials::directory()?;
    let input: input::Input = credentials::read(&directory, Filename::Keys)?;
    let (active_id, entries) = match input.try_into() {
        Ok(entries) => entries,
        Err(source) => {
            return Err(Error::Input {
                file: Filename::Keys,
                source,
            });
        }
    };
    match keys::Keys::new(active_id, entries) {
        Ok(keys) => Ok(keys),
        Err(source) => Err(Error::Keys {
            file: Filename::Keys,
            source,
        }),
    }
}

#[derive(Error)]
pub enum Error {
    #[error("{0}")]
    Credentials(#[from] credentials::Error),
    #[error("{source} in {file}")]
    Input {
        file: Filename,
        #[source]
        source: input::Error,
    },
    #[error("invalid key set in {file}: {source}")]
    Keys {
        file: Filename,
        #[source]
        source: keys::Error,
    },
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

    use crate::{
        credentials::{self, Filename},
        keys::credentials::{Error, load},
    };

    const KEY: &str = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;

    #[test]
    fn diagnostics_identify_invalid_key_sets_without_material() {
        let error = Error::Keys {
            file: Filename::Keys,
            source: domain::keys::Error::DuplicateId(domain::keys::Id::new(7).expect("id")),
        };
        let diagnostic = format!("{error:?}");
        assert!(diagnostic.contains("keys.json"));
        assert!(diagnostic.contains("duplicate key id Id(7)"));
        assert!(
            std::error::Error::source(&error)
                .expect("source")
                .downcast_ref::<domain::keys::Error>()
                .is_some()
        );
    }

    #[test]
    fn loads_the_fixed_root_file_without_supplier_or_application_settings() {
        if let Ok(case) = env::var("TEST_DEPLOYMENT_CREDENTIALS") {
            let result = load();
            match case.as_str() {
                "valid" => assert!(result.is_ok()),
                "missing" => {
                    let error = result.err().expect("missing fixed file rejected");
                    assert!(matches!(
                        &error,
                        Error::Credentials(credentials::Error::Read {
                            file: Filename::Keys,
                            ..
                        })
                    ));
                    let diagnostic = format!("{error:?}");
                    assert!(diagnostic.contains("keys.json"));
                    assert!(
                        !diagnostic.contains(
                            env::var("CREDENTIALS_DIRECTORY")
                                .expect("directory")
                                .as_str()
                        )
                    );
                }
                "invalid" => {
                    let error = result.err().expect("invalid fixture rejected");
                    assert!(!format!("{error:?}").contains("sensitive"));
                }
                "duplicate" => assert!(matches!(
                    result,
                    Err(Error::Keys {
                        file: Filename::Keys,
                        source: domain::keys::Error::DuplicateId(_)
                    })
                )),
                "inactive" => assert!(matches!(
                    result,
                    Err(Error::Keys {
                        file: Filename::Keys,
                        source: domain::keys::Error::Active(_)
                    })
                )),
                _ => panic!("unknown test case"),
            }
            return;
        }
        let directory = TempDir::new().expect("directory");
        let path = directory.path();
        // Former per-purpose files are not alternative sources.
        for file in ["challenge-keys.json", "mobile-keys.json", "seal-keys.json"] {
            fs::write(path.join(file), KEY).expect("ignored former file");
        }
        fs::write(path.join(".env"), "NUCLEUS_MIGRATION__PORT=invalid\n").expect("ignored env");
        let run = |case: &str| {
            let result = Command::new(env::current_exe().expect("executable"))
                .args([
                    "--exact",
                    "keys::credentials::tests::loads_the_fixed_root_file_without_supplier_or_application_settings",
                ])
                .env_clear()
                .current_dir(path)
                .env("TEST_DEPLOYMENT_CREDENTIALS", case)
                .env("CREDENTIALS_DIRECTORY", path)
                .output()
                .expect("isolated child");
            assert!(
                result.status.success(),
                "{case}: {} {}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        };
        run("missing");
        fs::write(path.join("keys.json"), KEY).expect("root");
        run("valid");
        let entry =
            serde_json::json!({"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"});
        fs::write(
            path.join("keys.json"),
            serde_json::to_vec(&serde_json::json!({"version":1,"active":0,"keys":[entry,entry]}))
                .expect("duplicate key JSON"),
        )
        .expect("duplicate root");
        run("duplicate");
        fs::write(
            path.join("keys.json"),
            KEY.replace(r#""active":0"#, r#""active":1"#),
        )
        .expect("undeclared active root");
        run("inactive");
        for invalid in [
            r#"[1,0,[]]"#,
            r#"{"version":2,"active":0,"keys":[]}"#,
            r#"{"version":1,"active":0,"keys":[[0,"sensitive"]]}"#,
            r#"{"version":1,"active":0,"keys":[{"id":0,"material":"sensitive"}]}"#,
            r#"{"version":1,"active":0,"keys":[],"sensitive":true}"#,
            r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8","purpose":"root"}]}"#,
        ] {
            fs::write(path.join("keys.json"), invalid).expect("invalid fixture");
            run("invalid");
        }
    }
}
