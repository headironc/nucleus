use std::process::ExitCode;

use clap::Parser;
use nucleus::{diagnostics, process};

#[derive(Parser)]
#[command(version, about = "Run the Nucleus service")]
struct Arguments {}

fn main() -> ExitCode {
    Arguments::parse();
    if let Err(error) = diagnostics::init() {
        eprintln!("diagnostics initialization failed: {error:?}");
        return ExitCode::FAILURE;
    }
    let error = match process::run() {
        Ok(()) => return ExitCode::SUCCESS,
        Err(error) => error,
    };
    tracing::error!(stage = error.stage(), kind = "process_failure", error = ?error, "process exited unsuccessfully");
    ExitCode::FAILURE
}
