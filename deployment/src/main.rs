use std::{env, process::ExitCode};

use deployment::{database, keys, migration, permissions, settings::Settings};
use tokio::runtime::Runtime;

enum Command {
    Migrate,
    InitializeKeys,
}

fn main() -> ExitCode {
    let arguments: Vec<_> = env::args_os().skip(1).collect();
    let command = match arguments.as_slice() {
        [argument] if argument == "migrate" => Command::Migrate,
        [argument] if argument == "initialize-keys" => Command::InitializeKeys,
        _ => {
            eprintln!("usage: deployment <migrate|initialize-keys>");
            return ExitCode::FAILURE;
        }
    };
    match run(command) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error:?}");
            ExitCode::FAILURE
        }
    }
}

fn run(command: Command) -> Result<(), Box<dyn std::error::Error>> {
    let settings = Settings::load()?;
    let runtime = Runtime::new()?;
    match command {
        Command::Migrate => {
            let runtime_user = settings.runtime_user().to_owned();
            let connections = database::load(settings.into_database())?;
            runtime.block_on(migration::run(&connections, &runtime_user))?;
            runtime.block_on(permissions::verify(&connections, &runtime_user))?;
        }
        Command::InitializeKeys => {
            let connections = database::load(settings.into_database())?;
            let metadata = keys::credentials::load()?;
            runtime.block_on(keys::initialize(&connections, &metadata))?;
        }
    }
    Ok(())
}
