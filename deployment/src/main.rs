use std::process::ExitCode;

use clap::{Parser, Subcommand};
use deployment::{database, instance, keys, migration, permissions, settings::Settings};
use tokio::runtime::Runtime;

#[derive(Parser)]
struct Arguments {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Migrate,
    Keys {
        #[command(subcommand)]
        action: Keys,
    },
    Admin {
        #[command(subcommand)]
        action: deployment::admin::Action,
    },
}

#[derive(Subcommand)]
enum Keys {
    Init,
}

fn main() -> ExitCode {
    match run(Arguments::parse().command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let runtime = Runtime::new()?;
    match command {
        Command::Migrate => {
            let settings = Settings::load()?;
            let runtime_user = settings.runtime_user().to_owned();
            let connections = database::load(settings.into_database())?;
            // Migration holds the exclusive runtime barrier for its session, so no
            // application can start, and advance a generation, until it ends.
            runtime.block_on(instance::exclusive(&connections, |_| async {
                migration::run(&connections, &runtime_user).await?;
                permissions::verify(&connections, &runtime_user).await?;
                Ok::<_, Migration>(())
            }))?;
        }
        Command::Keys { action: Keys::Init } => {
            let settings = Settings::load()?;
            let connections = database::load(settings.into_database())?;
            let metadata = keys::credentials::load()?;
            runtime.block_on(instance::exclusive(&connections, |fence| {
                keys::initialize(&connections, metadata.metadata(), fence)
            }))?;
        }
        Command::Admin { action } => runtime.block_on(action.run())?,
    }
    Ok(())
}

/// Migration and its permission check run under one instance lock.
#[derive(Debug, thiserror::Error)]
enum Migration {
    #[error("{0}")]
    Run(#[from] migration::Error),
    #[error("{0}")]
    Permissions(#[from] permissions::Error),
}

#[cfg(test)]
#[allow(clippy::expect_used)]
mod tests {
    use clap::Parser;

    use crate::Arguments;

    #[test]
    fn admin_requires_original_operation_and_private_input() {
        for action in ["initialize", "appoint", "correct", "correct-affiliation"] {
            let base = [
                "deployment",
                "admin",
                action,
                "--input",
                "input.json",
                "--operation",
                "00000000-0000-4000-8000-000000000001",
            ];
            Arguments::try_parse_from(base).expect("complete admin command");
            Arguments::try_parse_from(base.into_iter().chain(["--dry-run"]))
                .expect("validation without business writes");
            for omitted in ["--input", "--operation"] {
                let mut arguments = base[..3].to_vec();
                arguments.extend(
                    base[3..]
                        .chunks(2)
                        .filter(|pair| pair[0] != omitted)
                        .flatten()
                        .copied(),
                );
                assert!(
                    Arguments::try_parse_from(arguments).is_err(),
                    "{action} {omitted}"
                );
            }
            assert!(
                Arguments::try_parse_from(base.into_iter().chain(["--mobile", "13800000000"]))
                    .is_err()
            );
        }
        assert!(
            Arguments::try_parse_from([
                "deployment",
                "operate-access",
                "correct",
                "inspect",
                "--operator-file",
                "operator.json",
            ])
            .is_err(),
            "the retired offline command is not accepted"
        );
        Arguments::try_parse_from(["deployment", "keys", "init"]).expect("key metadata command");
    }
}
