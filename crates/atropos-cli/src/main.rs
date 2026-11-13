//! `atr.exe`: the Atropos command-line interface (console subsystem).

mod launch;

use std::process::ExitCode;

use clap::Parser;

/// Atropos: cut the right thread. Run with no arguments to open the GUI.
#[derive(Parser, Debug)]
#[command(name = "atr", version, about)]
struct Cli {}

fn main() -> ExitCode {
    let _cli = Cli::parse();
    let result = std::env::current_exe()
        .map_err(anyhow::Error::from)
        .and_then(|exe| launch::launch_gui(&exe));
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("atr: {e}");
            ExitCode::from(1)
        }
    }
}
