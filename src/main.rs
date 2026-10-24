use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nucleus::{diagnostics, process};

#[derive(Parser)]
#[command(version, about = "Run the Nucleus service")]
struct Arguments {
    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Check service process isolation before startup.
    Check,
}

fn main() -> ExitCode {
    let arguments = Arguments::parse();
    if let Err(error) = diagnostics::init() {
        eprintln!("diagnostics initialization failed: {error:?}");
        return ExitCode::FAILURE;
    }
    let result = match arguments.command {
        Some(Command::Check) => process::check(),
        None => process::run(),
    };
    let error = match result {
        Ok(()) => return ExitCode::SUCCESS,
        Err(error) => error,
    };
    tracing::error!(stage = error.stage(), kind = "process_failure", error = ?error, "process exited unsuccessfully");
    ExitCode::FAILURE
}
