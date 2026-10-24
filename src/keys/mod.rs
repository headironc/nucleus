//! Application loading of fixed, purpose-specific Identity key files.

pub mod challenge;
pub mod encryption;
pub mod message;
pub mod mobile;

use std::fmt;

use domain::identity::keys::{self, input};
use thiserror::Error;

use crate::credentials::{self, Filename};

fn read(file: Filename) -> Result<(keys::Id, Vec<keys::Entry>), Error> {
    let directory = credentials::directory()?;
    let input: input::Input = credentials::read(&directory, file)?;
    match input.try_into() {
        Ok(entries) => Ok(entries),
        Err(source) => Err(Error::Input { file, source }),
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

    use domain::identity::keys::input::{self, Input};
    use serde_json::{Value, json};
    use tempfile::TempDir;

    use crate::{
        credentials::{self, Filename},
        keys::{Error, challenge, encryption, mobile},
    };

    const JSON: &str = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;

    fn child(directory: &Path, case: &str) {
        let mut command = Command::new(env::current_exe().expect("test executable"));
        command
            .args([
                "--exact",
                "keys::tests::loads_only_fixed_file_from_exported_directory",
            ])
            .env_clear()
            .current_dir(directory)
            .env("TEST_NUCLEUS_KEYS_CHILD", case)
            .env("CREDENTIALS_DIRECTORY", directory)
            .env("NUCLEUS_CHALLENGE_KEYS", JSON);
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

    #[test]
    fn loads_only_fixed_file_from_exported_directory() {
        if let Ok(case) = env::var("TEST_NUCLEUS_KEYS_CHILD") {
            let result = challenge::load();
            match case.as_str() {
                "valid" => {
                    let keys = result.expect("fictional key file");
                    let token = " AbÇ-09\n".parse().expect("fictional token");
                    let proof = keys.active().prove(&token).expect("proof");
                    assert_eq!(
                        proof.as_ref(),
                        &[
                            69, 78, 35, 30, 176, 5, 72, 35, 164, 156, 220, 70, 157, 26, 75, 148,
                            27, 211, 189, 212, 150, 184, 8, 106, 23, 246, 155, 196, 129, 123, 131,
                            124
                        ]
                    );
                    assert!(
                        keys.fetch(keys.active().id())
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
                        file: Filename::ChallengeKeys,
                        ..
                    }))
                )),
                "invalid" | "invalid_active" | "invalid_id" | "invalid_second_material" => {
                    let error = match result {
                        Err(error) => error,
                        Ok(_) => panic!("invalid keys accepted"),
                    };
                    let diagnostic = format!("{error:?} {error}");
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
        fs::write(path.join("challenge-keys.json"), JSON).expect("fixture keys");
        fs::write(path.join(".env"), "CREDENTIALS_DIRECTORY=/not-loaded\n")
            .expect("ignored fixture env");
        for case in ["valid", "missing_directory", "relative_directory"] {
            child(path, case);
        }
        fs::remove_file(path.join("challenge-keys.json")).expect("remove fixture keys");
        child(path, "missing_file");
        fs::write(
            path.join("challenge-keys.json"),
            JSON.replace(
                "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
                "sensitive-material",
            ),
        )
        .expect("invalid fixture");
        child(path, "invalid");
        for (case, value) in [
            ("invalid_active", json!({"version":1,"active":-1,"keys":[]})),
            (
                "invalid_id",
                json!({"version":1,"active":0,"keys":[fixture()["keys"][0].clone(), {"id":-1,"material":"sensitive-material"}]}),
            ),
            (
                "invalid_second_material",
                json!({"version":1,"active":0,"keys":[fixture()["keys"][0].clone(), {"id":7,"material":"sensitive-material"}]}),
            ),
        ] {
            fs::write(
                path.join("challenge-keys.json"),
                serde_json::to_vec(&value).expect("fixture JSON"),
            )
            .expect("fixture");
            child(path, case);
        }
        let mut bytes = JSON.as_bytes().to_vec();
        bytes.resize(128 * 1024, b' ');
        fs::write(path.join("challenge-keys.json"), bytes).expect("padded fixture");
        child(path, "valid");
    }
    fn fixture() -> Value {
        serde_json::from_str(JSON).expect("fixture")
    }

    fn decode(bytes: &[u8]) -> Result<Input, credentials::Error> {
        let directory = TempDir::new().expect("fixture directory");
        fs::write(directory.path().join("challenge-keys.json"), bytes).expect("fixture");
        credentials::read(directory.path(), Filename::ChallengeKeys)
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
            assert!(diagnostic.contains("challenge-keys.json"), "{diagnostic}");
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
            ("/active", json!({"input_proof":0})),
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
        let mut value = fixture();
        value["keys"][0]["purpose"] = json!("challenge");
        assert!(matches!(
            rejected(&value),
            credentials::Error::Decode { .. }
        ));
        let mut bytes = serde_json::to_vec(&fixture()).expect("JSON");
        bytes.extend_from_slice(b" {}");
        assert!(matches!(
            decode(&bytes),
            Err(credentials::Error::Decode { .. })
        ));
    }

    #[test]
    fn loads_mobile_protection_from_separate_fixed_files() {
        if let Ok(case) = env::var("TEST_NUCLEUS_MOBILE_KEYS_CHILD") {
            let mobile_keys = mobile::load();
            let encryption_keys = encryption::load();
            match case.as_str() {
                "valid" => {
                    let mobile_keys = mobile_keys.expect("mobile_keys file");
                    let encryption_keys = encryption_keys.expect("encryption_keys file");
                    let mobile = "13800000000".parse().expect("mobile");
                    let challenge_id = "01890f6e-7b80-7000-8000-000000000000"
                        .parse()
                        .expect("challenge id");
                    let sealed = domain::identity::challenge::mobile::Sealed::seal(
                        &encryption_keys,
                        challenge_id,
                        &mobile,
                    )
                    .expect("seal");
                    let opened = sealed.open(&encryption_keys, challenge_id).expect("open");
                    assert_eq!(
                        mobile_keys.hash(&opened).expect("hash").as_ref(),
                        mobile_keys.hash(&mobile).expect("hash").as_ref()
                    );
                    assert_ne!(
                        mobile_keys
                            .fingerprints()
                            .next()
                            .expect("mobile_keys fingerprint")
                            .1
                            .as_ref(),
                        encryption_keys
                            .fingerprints()
                            .next()
                            .expect("encryption_keys fingerprint")
                            .1
                            .as_ref()
                    );
                }
                "missing_mobile_keys" => {
                    assert!(matches!(
                        mobile_keys,
                        Err(Error::Credentials(credentials::Error::Read {
                            file: Filename::MobileKeys,
                            ..
                        }))
                    ));
                    assert!(encryption_keys.is_ok());
                }
                "missing_encryption_keys" => {
                    assert!(mobile_keys.is_ok());
                    assert!(matches!(
                        encryption_keys,
                        Err(Error::Credentials(credentials::Error::Read {
                            file: Filename::EncryptionKeys,
                            ..
                        }))
                    ));
                }
                "invalid" => {
                    for (result, file) in [
                        (mobile_keys.err(), Filename::MobileKeys),
                        (encryption_keys.err(), Filename::EncryptionKeys),
                    ] {
                        let error = result.expect("invalid material");
                        assert!(
                            matches!(error, Error::Input { file: actual, source: input::Error::Material { .. } } if actual == file)
                        );
                        let diagnostic = format!("{error:?} {error}");
                        assert!(diagnostic.contains(file.as_ref()));
                        assert!(!diagnostic.contains("sensitive"));
                    }
                }
                "duplicate" => {
                    for (result, file) in [
                        (mobile_keys.err(), Filename::MobileKeys),
                        (encryption_keys.err(), Filename::EncryptionKeys),
                    ] {
                        assert!(
                            matches!(result, Some(Error::Keys { file: actual, source: domain::identity::keys::Error::DuplicateId(_) }) if actual == file)
                        );
                    }
                }
                _ => panic!("unknown test case"),
            }
            return;
        }
        let directory = TempDir::new().expect("fixture directory");
        let path = directory.path();
        let mobile_keys = path.join("mobile-keys.json");
        let encryption_keys = path.join("encryption-keys.json");
        let encryption_keys_json = JSON.replace(
            "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8",
            "KioqKioqKioqKioqKioqKioqKioqKioqKioqKioqKio",
        );
        fs::write(path.join("challenge-keys.json"), JSON).expect("unrelated fictional keys");
        fs::write(path.join(".env"), "CREDENTIALS_DIRECTORY=/not-loaded\n")
            .expect("ignored fixture env");
        for case in [
            "valid",
            "missing_mobile_keys",
            "missing_encryption_keys",
            "invalid",
            "duplicate",
        ] {
            fs::write(&mobile_keys, JSON).expect("mobile_keys fixture");
            fs::write(&encryption_keys, &encryption_keys_json).expect("encryption_keys fixture");
            match case {
                "missing_mobile_keys" => fs::remove_file(&mobile_keys).expect("remove fixture"),
                "missing_encryption_keys" => {
                    fs::remove_file(&encryption_keys).expect("remove fixture")
                }
                "invalid" | "duplicate" => {
                    let mut value = fixture();
                    if case == "invalid" {
                        value["keys"][0]["material"] = json!("sensitive-material");
                    } else {
                        let entry = value["keys"][0].clone();
                        value["keys"].as_array_mut().expect("entries").push(entry);
                    }
                    for file in [&mobile_keys, &encryption_keys] {
                        fs::write(file, serde_json::to_vec(&value).expect("fixture JSON"))
                            .expect("fixture");
                    }
                }
                _ => {}
            }
            let output = Command::new(env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "keys::tests::loads_mobile_protection_from_separate_fixed_files",
                ])
                .env_clear()
                .current_dir(path)
                .env("CREDENTIALS_DIRECTORY", path)
                .env("TEST_NUCLEUS_MOBILE_KEYS_CHILD", case)
                .output()
                .expect("isolated child");
            assert!(
                output.status.success(),
                "child {case}: {} {}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
