//! Application loading of the fixed root key file.

use std::fmt;

use domain::keys::{self, input};
use thiserror::Error;

use crate::credentials::{self, Filename};

/// Load the fixed root file and derive every key use without contacting any service.
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
    #[error("{source} in {file}")]
    Input {
        file: Filename,
        #[source]
        source: input::Error,
    },
    #[error("{0}")]
    Credentials(#[from] credentials::Error),
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
    use std::{env, fs, path::Path, process::Command};

    use domain::keys::input::{self, Input};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::{
        credentials::{self, Filename},
        keys::{Error, load},
    };

    const MATERIAL: &str = "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8";
    const JSON: &str = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;

    fn child(directory: &Path, case: &str) {
        let mut command = Command::new(env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "keys::tests::loads_only_the_fixed_root_file_from_exported_directory",
            ])
            .env_clear()
            .current_dir(directory)
            .env("TEST_NUCLEUS_KEYS_CHILD", case)
            .env("CREDENTIALS_DIRECTORY", directory)
            .env("NUCLEUS_KEYS", JSON);
        match case {
            "missing_directory" => {
                command.env_remove("CREDENTIALS_DIRECTORY");
            }
            "relative_directory" => {
                command.env("CREDENTIALS_DIRECTORY", ".");
            }
            _ => {}
        }
        let output = command.output().expect("isolated child");
        assert!(
            output.status.success(),
            "child {case}: {} {}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn write(directory: &Path, value: &Value) {
        fs::write(
            directory.join("keys.json"),
            serde_json::to_vec(value).expect("fixture JSON"),
        )
        .expect("fixture");
    }

    #[test]
    fn loads_only_the_fixed_root_file_from_exported_directory() {
        if let Ok(case) = env::var("TEST_NUCLEUS_KEYS_CHILD") {
            let result = load();
            match case.as_str() {
                "valid" => {
                    let keys = result.expect("fictional root file");
                    let challenge = &keys.identity.challenge;
                    let token = " AbÇ-09\n".parse().expect("fictional token");
                    let proof = challenge.active().prove(&token).expect("proof");
                    // Python: HKDF-SHA-256(salt "nucleus/key/v1", root bytes 0..31, info "lookup"),
                    // then hmac.digest over LP32("nucleus/identity/challenge-token/v1", token).
                    assert_eq!(
                        proof.as_ref(),
                        &[
                            200, 136, 41, 23, 70, 55, 137, 197, 223, 81, 47, 178, 185, 231, 164,
                            146, 106, 192, 179, 25, 225, 232, 214, 85, 83, 123, 56, 187, 7, 116,
                            89, 43
                        ]
                    );
                    assert!(
                        challenge
                            .fetch(challenge.active().id())
                            .expect("original version")
                            .verify(&token, &proof)
                            .expect("verify")
                    );
                }
                "missing_directory" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::DirectoryMissing))
                )),
                "relative_directory" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::DirectoryInvalid))
                )),
                "missing_file" => assert!(matches!(
                    result,
                    Err(Error::Credentials(credentials::Error::Read {
                        file: Filename::Keys,
                        ..
                    }))
                )),
                "duplicate_id" => assert!(matches!(
                    result,
                    Err(Error::Keys {
                        file: Filename::Keys,
                        source: domain::keys::Error::DuplicateId(_),
                    })
                )),
                "duplicate_material" => assert!(matches!(
                    result,
                    Err(Error::Keys {
                        file: Filename::Keys,
                        source: domain::keys::Error::DuplicateMaterial { .. },
                    })
                )),
                "undeclared_active" => assert!(matches!(
                    result,
                    Err(Error::Keys {
                        file: Filename::Keys,
                        source: domain::keys::Error::Active(_),
                    })
                )),
                "invalid" | "invalid_active" | "invalid_id" | "invalid_second_material" => {
                    let error = match result {
                        Err(error) => error,
                        Ok(_) => panic!("invalid keys accepted"),
                    };
                    let diagnostic = format!("{error:?} {error}");
                    assert!(diagnostic.contains("keys.json"));
                    match case.as_str() {
                        "invalid_active" => {
                            assert!(matches!(
                                error,
                                Error::Input {
                                    source: input::Error::Active(_),
                                    ..
                                }
                            ));
                            assert!(diagnostic.contains("at active"));
                        }
                        "invalid_id" => {
                            assert!(matches!(
                                error,
                                Error::Input {
                                    source: input::Error::Id { index: 1, .. },
                                    ..
                                }
                            ));
                            assert!(diagnostic.contains("keys[1].id"));
                        }
                        "invalid_second_material" => {
                            assert!(
                                matches!(error, Error::Input { source: input::Error::Material { index: 1, key_id, .. }, .. } if *key_id.as_ref() == 7)
                            );
                            assert!(diagnostic.contains("keys[1].material"));
                        }
                        _ => assert!(matches!(
                            error,
                            Error::Input {
                                source: input::Error::Material { index: 0, .. },
                                ..
                            }
                        )),
                    }
                    assert!(!diagnostic.contains("sensitive"));
                    assert!(std::error::Error::source(&error).is_some());
                }
                _ => panic!("unknown fixture case"),
            }
            return;
        }
        let directory = TempDir::new().expect("fixture directory");
        let path = directory.path();
        fs::write(path.join("challenge.json"), JSON).expect("unrelated fictional credential");
        // The former per-purpose files are not alternative sources.
        for file in [
            "challenge-keys.json",
            "encryption-keys.json",
            "seal-keys.json",
        ] {
            fs::write(path.join(file), JSON).expect("ignored former key file");
        }
        fs::write(path.join("keys.json"), JSON).expect("fixture keys");
        fs::write(path.join(".env"), "CREDENTIALS_DIRECTORY=/not-loaded\n")
            .expect("ignored fixture env");
        for case in ["valid", "missing_directory", "relative_directory"] {
            child(path, case);
        }
        fs::remove_file(path.join("keys.json")).expect("remove fixture keys");
        child(path, "missing_file");
        fs::write(
            path.join("keys.json"),
            JSON.replace(MATERIAL, "sensitive-material"),
        )
        .expect("invalid fixture");
        child(path, "invalid");
        let entry = fixture()["keys"][0].clone();
        for (case, value) in [
            ("invalid_active", json!({"version":1,"active":-1,"keys":[]})),
            (
                "invalid_id",
                json!({"version":1,"active":0,"keys":[entry, {"id":-1,"material":"sensitive-material"}]}),
            ),
            (
                "invalid_second_material",
                json!({"version":1,"active":0,"keys":[entry, {"id":7,"material":"sensitive-material"}]}),
            ),
            (
                "duplicate_id",
                json!({"version":1,"active":0,"keys":[entry, {"id":0,"material":"KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio"}]}),
            ),
            (
                "duplicate_material",
                json!({"version":1,"active":0,"keys":[entry, {"id":1,"material":MATERIAL}]}),
            ),
            (
                "undeclared_active",
                json!({"version":1,"active":1,"keys":[entry]}),
            ),
        ] {
            write(path, &value);
            child(path, case);
        }
        let mut bytes = JSON.as_bytes().to_vec();
        bytes.resize(128 * 1024, b' ');
        fs::write(path.join("keys.json"), bytes).expect("padded fixture");
        child(path, "valid");
    }

    fn fixture() -> Value {
        serde_json::from_str(JSON).expect("fixture")
    }

    fn decode(bytes: &[u8]) -> Result<Input, credentials::Error> {
        let directory = TempDir::new().expect("fixture directory");
        fs::write(directory.path().join("keys.json"), bytes).expect("fixture");
        credentials::read(directory.path(), Filename::Keys)
    }

    fn rejected(value: &Value) -> credentials::Error {
        match decode(&serde_json::to_vec(value).expect("JSON")) {
            Err(error) => error,
            Ok(_) => panic!("invalid input accepted"),
        }
    }

    #[test]
    fn rejects_noncanonical_material_without_disclosing_values() {
        let mut value = fixture();
        for material in [json!(12345678), json!({"sensitive":1}), Value::Null] {
            value["keys"][0]["material"] = material;
            let error = rejected(&value);
            let diagnostic = format!("{error:?} {error}");
            assert!(diagnostic.contains("keys.json"), "{diagnostic}");
            assert!(!diagnostic.contains("sensitive"));
            assert!(!diagnostic.contains("12345678"));
            assert!(std::error::Error::source(&error).is_some());
        }
    }

    #[test]
    fn requires_exact_object_schema_and_one_json_document() {
        for bytes in [
            b"".as_slice(),
            b"[]",
            b"null",
            &[0xff],
            b"{} {}",
            br#"{"version":1,"version":1,"active":0,"keys":[]}"#,
            br#"{"version":1,"active":0,"active":0,"keys":[]}"#,
            br#"{"version":1,"active":0,"keys":[{"id":0,"id":0,"material":"bad"}]}"#,
        ] {
            assert!(matches!(
                decode(bytes),
                Err(credentials::Error::Decode { .. })
            ));
        }
        for path in [
            "/version",
            "/active",
            "/keys",
            "/keys/0/id",
            "/keys/0/material",
        ] {
            let mut value = fixture();
            *value.pointer_mut(path).expect("fixture field") = Value::Null;
            assert!(matches!(
                rejected(&value),
                credentials::Error::Decode { .. }
            ));
        }
        for field in ["version", "active", "keys"] {
            let mut value = fixture();
            value.as_object_mut().expect("object").remove(field);
            assert!(matches!(
                rejected(&value),
                credentials::Error::Decode { .. }
            ));
        }
        for (path, replacement) in [
            ("/version", json!(2)),
            ("/version", json!(1.0)),
            ("/active", json!([0])),
            ("/active", json!({"input":0})),
            ("/keys/0", json!([0, "A".repeat(43)])),
            ("/keys/0/id", json!(u64::MAX)),
            ("/keys/0/id", json!(1.0)),
            ("/keys/0/id", json!("sensitive-id")),
        ] {
            let mut value = fixture();
            *value.pointer_mut(path).expect("fixture field") = replacement;
            assert!(matches!(
                rejected(&value),
                credentials::Error::Decode { .. }
            ));
        }
        for path in ["", "/keys/0"] {
            let mut value = fixture();
            value.pointer_mut(path).expect("object")["sensitive-unknown-field"] =
                json!("sensitive-value");
            let error = rejected(&value);
            assert!(!format!("{error:?} {error}").contains("sensitive"));
        }
        for field in ["purpose", "use"] {
            let mut value = fixture();
            value["keys"][0][field] = json!("root");
            assert!(matches!(
                rejected(&value),
                credentials::Error::Decode { .. }
            ));
        }
        let mut bytes = serde_json::to_vec(&fixture()).expect("JSON");
        bytes.extend_from_slice(b" {}");
        assert!(matches!(
            decode(&bytes),
            Err(credentials::Error::Decode { .. })
        ));
    }
}
