#![allow(clippy::expect_used)]

use std::{
    fs::{self, File},
    process::{Output, Stdio},
    time::Duration,
};

use tokio::{process::Command, time::timeout};

async fn run(arguments: &[&str]) -> Output {
    let directory = tempfile::tempdir().expect("isolated CLI directory");
    let stdout = directory.path().join("stdout");
    let stderr = directory.path().join("stderr");
    // Files keep output collection independent of pipe capacity and cancellation.
    let mut child = Command::new(env!("CARGO_BIN_EXE_nucleus"))
        .args(arguments)
        .current_dir(directory.path())
        .env_clear()
        .env("NUCLEUS_DATABASE__PORT", "fictional-invalid-port")
        .env("CREDENTIALS_DIRECTORY", directory.path().join("missing"))
        .stdin(Stdio::null())
        .stdout(File::create(&stdout).expect("CLI stdout capture"))
        .stderr(File::create(&stderr).expect("CLI stderr capture"))
        .kill_on_drop(true)
        .spawn()
        .expect("start the delivered binary");
    let status = match timeout(Duration::from_secs(3), child.wait()).await {
        Ok(result) => result.expect("confirmed CLI exit"),
        Err(_) => {
            child
                .kill()
                .await
                .expect("terminate and reap the stalled CLI");
            panic!("CLI exceeded its exit budget for {arguments:?}");
        }
    };
    Output {
        status,
        stdout: fs::read(stdout).expect("captured CLI stdout"),
        stderr: fs::read(stderr).expect("captured CLI stderr"),
    }
}

#[tokio::test]
async fn help_and_version_succeed_without_deployment_configuration() {
    let cases: &[(&[&str], &str)] = &[
        (&["--help"], "Usage: nucleus"),
        (&["check", "--help"], "Usage: nucleus check"),
    ];
    for (arguments, usage) in cases {
        let output = run(arguments).await;
        assert_eq!(output.status.code(), Some(0), "{arguments:?}: {output:?}");
        assert!(output.stderr.is_empty(), "{arguments:?}: {output:?}");
        let text = String::from_utf8(output.stdout).expect("UTF-8 help");
        assert!(text.contains(usage), "{arguments:?}: {text}");
    }

    let output = run(&["--version"]).await;
    assert_eq!(output.status.code(), Some(0), "{output:?}");
    assert!(output.stderr.is_empty(), "{output:?}");
    let text = String::from_utf8(output.stdout).expect("UTF-8 version");
    assert_eq!(
        text.trim(),
        concat!(env!("CARGO_PKG_NAME"), " ", env!("CARGO_PKG_VERSION"))
    );
}

#[tokio::test]
async fn invalid_arguments_exit_with_usage_status_without_echoing_input() {
    const PRIVATE: &str = "fictional-private-cli-value";
    let cases: &[&[&str]] = &[
        &["--fictional-private-cli-value"],
        &[PRIVATE],
        &["check", PRIVATE],
    ];
    for arguments in cases {
        let output = run(arguments).await;
        assert_eq!(output.status.code(), Some(2), "{arguments:?}: {output:?}");
        assert!(output.stdout.is_empty(), "{arguments:?}: {output:?}");
        let text = String::from_utf8(output.stderr).expect("UTF-8 argument diagnostic");
        assert!(!text.is_empty(), "{arguments:?}: missing diagnostic");
        assert!(!text.contains(PRIVATE), "{arguments:?}: {text}");
    }
}
