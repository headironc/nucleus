use domain::identity::keys::message::Keys;

use crate::{
    credentials::Filename,
    keys::{self, Error},
};

/// Load the fixed message code-proof key file without contacting any service.
pub fn load() -> Result<Keys, Error> {
    let (active_id, entries) = keys::read(Filename::MessageKeys)?;
    match Keys::new(active_id, entries) {
        Ok(keys) => Ok(keys),
        Err(source) => Err(Error::Keys {
            file: Filename::MessageKeys,
            source,
        }),
    }
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use std::{env, fs, process::Command};

    use super::load;

    #[test]
    fn loads_only_the_fixed_message_file_and_binds_its_proof() {
        if let Ok(case) = env::var("TEST_NUCLEUS_MESSAGE_KEYS") {
            let result = load();
            if case == "missing" {
                assert!(matches!(
                    result,
                    Err(crate::keys::Error::Credentials(
                        crate::credentials::Error::Read {
                            file: crate::credentials::Filename::MessageKeys,
                            ..
                        }
                    ))
                ));
                return;
            }
            let keys = result.expect("message keys");
            let message_id = domain::identity::message::Id::new();
            let code = "00001234".parse().expect("code");
            let proof = keys.active().prove(message_id, &code).expect("proof");
            assert!(
                keys.fetch(keys.active().id())
                    .expect("original key")
                    .verify(message_id, &code, &proof)
                    .expect("verify")
            );
            return;
        }
        let directory = tempfile::tempdir().expect("fixture directory");
        let json = r#"{"version":1,"active":0,"keys":[{"id":0,"material":"AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8"}]}"#;
        fs::write(directory.path().join("challenge-keys.json"), json)
            .expect("unrelated purpose does not substitute");
        for case in ["missing", "valid"] {
            if case == "valid" {
                fs::write(directory.path().join("message-keys.json"), json)
                    .expect("fictional message keys");
            }
            let output = Command::new(env::current_exe().expect("test executable"))
                .args([
                    "--exact",
                    "keys::message::tests::loads_only_the_fixed_message_file_and_binds_its_proof",
                ])
                .env_clear()
                .current_dir(directory.path())
                .env("CREDENTIALS_DIRECTORY", directory.path())
                .env("TEST_NUCLEUS_MESSAGE_KEYS", case)
                .output()
                .expect("isolated loader child");
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }
}
