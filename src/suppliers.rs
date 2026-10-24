//! Startup assembly of the two suppliers used by C1.

use std::fmt::{self, Formatter};

use domain::identity::{
    challenge,
    message::{self, credentials::AccessKey},
};
use serde::Deserialize;
use thiserror::Error;
use zeroize::Zeroizing;

use crate::credentials::{self, Filename, deserialize_version, read};

/// Reusable clients returned only after both suppliers initialize successfully.
#[derive(Debug)]
pub struct Clients {
    challenge: challenge::client::Client,
    message: message::client::Client,
}

impl Clients {
    /// Load fixed files from CREDENTIALS_DIRECTORY and inject validated settings.
    /// Construction makes no supplier calls and does not establish readiness.
    pub fn new(
        challenge: challenge::settings::Settings,
        message: message::settings::Settings,
    ) -> Result<Self, Error> {
        let directory = credentials::directory()?;
        let Challenge {
            version: (),
            mut secret,
        } = read::<Challenge>(&directory, Filename::Challenge)?;
        let challenge = challenge::client::Client::new(
            challenge,
            challenge::credentials::Secret::new(std::mem::take(&mut *secret))?,
        )?;
        let Message {
            version: (),
            mut id,
            mut secret,
        } = read::<Message>(&directory, Filename::Message)?;
        let message = message::client::Client::new(
            message,
            AccessKey::new(
                message::credentials::Id::new(std::mem::take(&mut *id))?,
                message::credentials::Secret::new(std::mem::take(&mut *secret))?,
            ),
        )?;
        Ok(Self { challenge, message })
    }

    /// Borrow the single-attempt Siteverify client.
    pub fn challenge(&self) -> &challenge::client::Client {
        &self.challenge
    }

    /// Borrow the client that constructs and executes signed PNVS requests.
    pub fn message(&self) -> &message::client::Client {
        &self.message
    }

    /// Transfer both initialized clients to their consuming services.
    pub fn into_parts(self) -> (challenge::client::Client, message::client::Client) {
        (self.challenge, self.message)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Challenge {
    #[serde(deserialize_with = "deserialize_version")]
    version: (),
    secret: Zeroizing<String>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Message {
    #[serde(deserialize_with = "deserialize_version")]
    version: (),
    id: Zeroizing<String>,
    secret: Zeroizing<String>,
}

/// Startup failure with controlled diagnostics; source details must not be logged.
#[derive(Error)]
pub enum Error {
    #[error("{0}")]
    ChallengeSecret(#[from] challenge::credentials::ParseError),
    #[error("{0}")]
    MessageId(#[from] message::credentials::IdError),
    #[error("{0}")]
    MessageSecret(#[from] message::credentials::SecretError),
    #[error("{0}")]
    Credentials(#[from] credentials::Error),
    #[error("cannot initialize the Siteverify client: {0}")]
    Challenge(#[from] challenge::client::BuildError),
    #[error("cannot initialize the PNVS client")]
    Message(#[from] reqwest::Error),
}

impl fmt::Debug for Error {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        fmt::Display::fmt(self, formatter)
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{env, fs, process::Command};

    use domain::identity::{challenge, message};
    use nonempty::NonEmpty;
    use serde::de::DeserializeOwned;
    use tempfile::TempDir;

    use crate::{
        credentials::{Error, Filename, read},
        suppliers::{self, Challenge, Clients, Message},
    };

    const CHALLENGE: &str = r#"{"version":1,"secret":"test-challenge-secret"}"#;
    const MESSAGE: &str = r#"{"version":1,"id":"TestAccessKey123","secret":"test-message-secret"}"#;

    struct Directory(TempDir);

    impl Directory {
        fn new() -> Self {
            Self(TempDir::new().expect("unique temporary directory is created"))
        }

        fn write(&self, name: &str, bytes: &[u8]) {
            fs::write(self.0.path().join(name), bytes).expect("synthetic fixture is written");
        }

        fn credentials(&self) {
            self.write("challenge.json", CHALLENGE.as_bytes());
            self.write("message.json", MESSAGE.as_bytes());
        }
    }

    fn challenge_settings() -> challenge::settings::Settings {
        challenge::settings::Settings::new(
            challenge::settings::AllowedHostnames::new(NonEmpty::new("example.com".to_owned()))
                .expect("synthetic hostname is valid"),
        )
    }

    fn message_settings() -> message::settings::Settings {
        use domain::identity::message::settings::{
            DEFAULT_TIMEOUT_MILLISECONDS, Signature, TemplateCode, Templates,
        };
        message::settings::Settings::new(
            Signature::new("Test Signature".into()).expect("valid signature"),
            Templates::new(
                TemplateCode::new("TestAuth".into()).expect("valid template"),
                TemplateCode::new("TestChange".into()).expect("valid template"),
                TemplateCode::new("TestBinding".into()).expect("valid template"),
            ),
            DEFAULT_TIMEOUT_MILLISECONDS,
        )
        .expect("synthetic message settings are valid")
    }

    #[test]
    fn preserves_credential_text() {
        let directory = Directory::new();
        directory.credentials();
        let challenge: Challenge =
            read(directory.0.path(), Filename::Challenge).expect("valid challenge file");
        let message: Message =
            read(directory.0.path(), Filename::Message).expect("valid message file");
        assert_eq!(challenge.secret.as_str(), "test-challenge-secret");
        assert_eq!(message.id.as_str(), "TestAccessKey123");
        assert_eq!(message.secret.as_str(), "test-message-secret");
    }

    #[test]
    fn rejects_invalid_versions_duplicates_and_unknown_fields() {
        fn check<T: DeserializeOwned>(name: Filename, json: &str) {
            let directory = Directory::new();
            for version in ["0", "2", "-1", "1.0", "1e0", "true", "null", "\"1\""] {
                directory.write(
                    name.as_ref(),
                    json.replace("\"version\":1", &format!("\"version\":{version}"))
                        .as_bytes(),
                );
                assert!(matches!(
                    read::<T>(directory.0.path(), name),
                    Err(Error::Decode { .. })
                ));
            }
            for invalid in [
                json.replace("\"version\":1,", ""),
                json.replace("\"version\":1", "\"version\":1,\"version\":1"),
                json.replace("\"secret\":", "\"secret\":\"duplicate-secret\",\"secret\":"),
                json.replace("\"version\":1", "\"version\":1,\"unexpected\":true"),
                format!("{json} {{}}"),
            ] {
                directory.write(name.as_ref(), invalid.as_bytes());
                assert!(matches!(
                    read::<T>(directory.0.path(), name),
                    Err(Error::Decode { .. })
                ));
            }
            for invalid in [
                b"".as_slice(),
                b"{}",
                b"null",
                b"[]",
                b"{",
                &[0xff, 0xfe],
                br#"[1,"test-challenge-secret"]"#,
                br#"[1,"TestAccessKey123","test-message-secret"]"#,
            ] {
                directory.write(name.as_ref(), invalid);
                assert!(matches!(
                    read::<T>(directory.0.path(), name),
                    Err(Error::Decode { .. })
                ));
            }
        }
        check::<Challenge>(Filename::Challenge, CHALLENGE);
        check::<Message>(Filename::Message, MESSAGE);
    }

    #[test]
    fn rejects_missing_fields_and_wrong_types() {
        fn check<T: DeserializeOwned>(name: Filename, json: &str) {
            let directory = Directory::new();
            let value: serde_json::Value = serde_json::from_str(json).expect("valid fixture JSON");
            for field in if name == Filename::Challenge {
                &["secret"][..]
            } else {
                &["id", "secret"][..]
            } {
                let mut missing = value.clone();
                missing
                    .as_object_mut()
                    .expect("fixture is an object")
                    .remove(*field);
                directory.write(
                    name.as_ref(),
                    &serde_json::to_vec(&missing).expect("fixture serializes"),
                );
                assert!(matches!(
                    read::<T>(directory.0.path(), name),
                    Err(Error::Decode { .. })
                ));
                for invalid in [serde_json::Value::Null, serde_json::json!(123)] {
                    let mut invalid_file = value.clone();
                    invalid_file[*field] = invalid;
                    directory.write(
                        name.as_ref(),
                        &serde_json::to_vec(&invalid_file).expect("fixture serializes"),
                    );
                    assert!(matches!(
                        read::<T>(directory.0.path(), name),
                        Err(Error::Decode { .. })
                    ));
                }
            }
        }
        check::<Challenge>(Filename::Challenge, CHALLENGE);
        check::<Message>(Filename::Message, MESSAGE);
        let directory = Directory::new();
        let json = MESSAGE.replace("\"id\":", "\"id\":\"Duplicate123\",\"id\":");
        directory.write("message.json", json.as_bytes());
        assert!(matches!(
            read::<Message>(directory.0.path(), Filename::Message),
            Err(Error::Decode { .. })
        ));
    }

    #[test]
    fn diagnostics_hide_values_and_paths_but_retain_error_sources() {
        fn check<T: DeserializeOwned>(name: Filename) {
            let directory = Directory::new();
            directory.write(
                name.as_ref(),
                br#"{"sensitive-field-marker":"sensitive-value-marker"}"#,
            );
            let error = read::<T>(directory.0.path(), name)
                .map(drop)
                .expect_err("unknown field fails");
            let diagnostic = format!("{error:?} {error}");
            assert!(diagnostic.contains(name.as_ref()));
            assert!(!diagnostic.contains("sensitive-field-marker"));
            assert!(!diagnostic.contains("sensitive-value-marker"));
            assert!(std::error::Error::source(&error).is_some());
            fs::remove_file(directory.0.path().join(name.as_ref())).expect("remove synthetic file");
            let error = read::<T>(directory.0.path(), name)
                .map(drop)
                .expect_err("missing file fails");
            assert!(
                matches!(&error, Error::Read { source, .. } if source.kind() == std::io::ErrorKind::NotFound)
            );
            assert!(
                !format!("{error:?} {error}")
                    .contains(directory.0.path().to_str().expect("test path is UTF-8"))
            );
            assert!(std::error::Error::source(&error).is_some());
        }
        check::<Challenge>(Filename::Challenge);
        check::<Message>(Filename::Message);
    }

    #[test]
    fn loads_only_from_the_exported_directory() {
        // Child processes exercise the real environment entrypoint without
        // mutating this multithreaded test process's environment.
        if let Some(case) = env::var_os("TEST_NUCLEUS_SUPPLIERS_CHILD") {
            let case = case.to_str().expect("fixed test case");
            let result = Clients::new(challenge_settings(), message_settings());
            match case {
                "valid" => {
                    let clients = result.expect("exported fixture directory loads");
                    let diagnostic = format!(
                        "{clients:?} {:?} {:?}",
                        clients.challenge(),
                        clients.message()
                    );
                    for marker in [
                        "TestAccessKey123",
                        "test-challenge-secret",
                        "test-message-secret",
                    ] {
                        assert!(!diagnostic.contains(marker));
                    }
                    let (challenge, _message) = clients.into_parts();
                    let result = tokio::runtime::Runtime::new().expect("runtime").block_on(
                        challenge.verify(
                            &"fictional-token".parse().expect("token"),
                            challenge::Id::new(),
                            challenge::Purpose::Authentication,
                            "00000000-0000-0000-0000-000000000000"
                                .parse()
                                .expect("operation"),
                            std::time::Duration::ZERO,
                        ),
                    );
                    assert!(matches!(result, Err(challenge::client::Error::Expired)));
                }
                "missing" => assert!(matches!(
                    result,
                    Err(suppliers::Error::Credentials(Error::DirectoryMissing))
                )),
                "empty" | "current" | "relative" => {
                    assert!(matches!(
                        result,
                        Err(suppliers::Error::Credentials(Error::DirectoryInvalid))
                    ));
                }
                "file_missing" => assert!(matches!(
                    result,
                    Err(suppliers::Error::Credentials(Error::Read {
                        file: Filename::Message,
                        ..
                    }))
                )),
                "challenge_invalid" => {
                    assert!(matches!(result, Err(suppliers::Error::ChallengeSecret(_))))
                }
                "message_id_invalid" => {
                    assert!(matches!(result, Err(suppliers::Error::MessageId(_))))
                }
                "message_secret_invalid" => {
                    assert!(matches!(result, Err(suppliers::Error::MessageSecret(_))))
                }
                _ => panic!("unknown test case"),
            }
            return;
        }
        let directory = Directory::new();
        directory.credentials();
        directory.write(".env", b"CREDENTIALS_DIRECTORY=/not-loaded\n");
        for case in [
            "valid",
            "missing",
            "empty",
            "current",
            "relative",
            "file_missing",
            "challenge_invalid",
            "message_id_invalid",
            "message_secret_invalid",
        ] {
            directory.credentials();
            let mut command = Command::new(env::current_exe().expect("test executable exists"));
            command
                .args([
                    "--exact",
                    "suppliers::tests::loads_only_from_the_exported_directory",
                ])
                .current_dir(directory.0.path())
                .env_clear()
                .env("TEST_NUCLEUS_SUPPLIERS_CHILD", case)
                .env("CREDENTIALS_DIRECTORY", directory.0.path())
                .env("NUCLEUS_MESSAGE__SECRET", "ignored-env-secret");
            match case {
                "missing" => {
                    command.env_remove("CREDENTIALS_DIRECTORY");
                }
                "empty" => {
                    command.env("CREDENTIALS_DIRECTORY", "");
                }
                "current" => {
                    command.env("CREDENTIALS_DIRECTORY", ".");
                }
                "relative" => {
                    command.env("CREDENTIALS_DIRECTORY", "relative/credentials");
                }
                "file_missing" => {
                    fs::remove_file(directory.0.path().join("message.json"))
                        .expect("remove synthetic credential");
                }
                "challenge_invalid" => directory.write(
                    "challenge.json",
                    CHALLENGE
                        .replace("test-challenge-secret", "bad secret")
                        .as_bytes(),
                ),
                "message_id_invalid" => directory.write(
                    "message.json",
                    MESSAGE.replace("TestAccessKey123", "invalid_id").as_bytes(),
                ),
                "message_secret_invalid" => directory.write(
                    "message.json",
                    MESSAGE
                        .replace("test-message-secret", "bad secret")
                        .as_bytes(),
                ),
                _ => {}
            }
            let output = command.output().expect("isolated test process starts");
            assert!(
                output.status.success(),
                "child failed: {}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
