//! Deployment key file decoding and validated metadata assembly.

use std::{fmt, path::Path};

use domain::identity::keys::{
    self, challenge, encryption, input, message, metadata::Metadata, mobile,
};
use thiserror::Error;

use crate::credentials::{self, Filename};

/// Derive metadata from the same validated purpose types used by the application.
pub fn load() -> Result<Metadata, Error> {
    let directory = credentials::directory()?;
    let (active_id, entries) = read_entries(&directory, Filename::ChallengeKeys)?;
    let challenge = match challenge::Keys::new(active_id, entries) {
        Ok(keys) => keys,
        Err(source) => {
            return Err(Error::Keys {
                file: Filename::ChallengeKeys,
                source,
            });
        }
    };
    let (active_id, entries) = read_entries(&directory, Filename::MobileKeys)?;
    let mobile = match mobile::Keys::new(active_id, entries) {
        Ok(keys) => keys,
        Err(source) => {
            return Err(Error::Keys {
                file: Filename::MobileKeys,
                source,
            });
        }
    };
    let (active_id, entries) = read_entries(&directory, Filename::EncryptionKeys)?;
    let encryption = match encryption::Keys::new(active_id, entries) {
        Ok(keys) => keys,
        Err(source) => {
            return Err(Error::Keys {
                file: Filename::EncryptionKeys,
                source,
            });
        }
    };
    let (active_id, entries) = read_entries(&directory, Filename::MessageKeys)?;
    let message = match message::Keys::new(active_id, entries) {
        Ok(keys) => keys,
        Err(source) => {
            return Err(Error::Keys {
                file: Filename::MessageKeys,
                source,
            });
        }
    };
    Ok(Metadata::new(&challenge, &mobile, &encryption, &message))
}

fn read_entries(directory: &Path, file: Filename) -> Result<(keys::Id, Vec<keys::Entry>), Error> {
    let input: input::Input = credentials::read(directory, file)?;
    match input.try_into() {
        Ok(entries) => Ok(entries),
        Err(source) => Err(Error::Input { file, source }),
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

    #[test]
    fn diagnostics_identify_invalid_key_sets_without_material() {
        let error = crate::keys::credentials::Error::Keys {
            file: crate::credentials::Filename::ChallengeKeys,
            source: domain::identity::keys::Error::DuplicateId(
                domain::identity::keys::Id::new(7).expect("id"),
            ),
        };
        let diagnostic = format!("{error:?}");
        assert!(diagnostic.contains("challenge-keys.json"));
        assert!(diagnostic.contains("duplicate key id Id(7)"));
        assert!(
            std::error::Error::source(&error)
                .expect("source")
                .downcast_ref::<domain::identity::keys::Error>()
                .is_some()
        );
    }

    use crate::{
        credentials::{self, Filename},
        keys::credentials::{Error, load},
    };

    const KEY: &str = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;

    #[test]
    fn loads_fixed_files_without_supplier_or_application_settings() {
        if let Ok(case) = env::var("TEST_DEPLOYMENT_CREDENTIALS") {
            let result = load();
            match case.as_str() {
                "valid" => assert!(result.is_ok()),
                "missing" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::Read {
                        file: Filename::MobileKeys,
                        ..
                    }))
                )),
                "invalid" => {
                    let error = result.err().expect("invalid fixture rejected");
                    assert!(!format!("{error:?}").contains("sensitive"));
                }
                _ => panic!("unknown test case"),
            }
            return;
        }
        let directory = TempDir::new().expect("directory");
        let path = directory.path();
        fs::write(path.join("challenge-keys.json"), KEY).expect("challenge");
        fs::write(path.join("encryption-keys.json"), KEY).expect("encryption");
        fs::write(path.join("message-keys.json"), KEY).expect("message");
        fs::write(path.join(".env"), "NUCLEUS_MIGRATION__PORT=invalid\n").expect("ignored env");
        let run = |case: &str| {
            let result = Command::new(env::current_exe().expect("executable"))
                .args([
                    "--exact",
                    "keys::credentials::tests::loads_fixed_files_without_supplier_or_application_settings",
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
        fs::write(path.join("mobile-keys.json"), KEY).expect("mobile");
        run("valid");
        for invalid in [
            r#"[1,0,[]]"#,
            r#"{"version":2,"active":0,"keys":[]}"#,
            r#"{"version":1,"active":0,"keys":[[0,"sensitive"]]}"#,
            r#"{"version":1,"active":0,"keys":[{"id":0,"material":"sensitive"}]}"#,
            r#"{"version":1,"active":0,"keys":[],"sensitive":true}"#,
        ] {
            fs::write(path.join("mobile-keys.json"), invalid).expect("invalid fixture");
            run("invalid");
        }
    }
}
