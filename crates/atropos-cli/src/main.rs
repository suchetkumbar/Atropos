//! `atr.exe`: the Atropos command-line interface (console subsystem).

mod launch;
mod run;

use std::io::{BufRead as _, IsTerminal as _, Write as _};
use std::process::ExitCode;
use std::time::Duration;

use clap::{Args, Parser, Subcommand};

use run::{Env, KillArgs, parse_duration};

/// Atropos: cut the right thread. Run with no arguments to open the GUI.
#[derive(Parser, Debug)]
#[command(name = "atr", version, about)]
struct Cli {
    /// Machine-readable output (`"schema": 1`).
    #[arg(long, global = true)]
    json: bool,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Args, Debug, Clone, Copy)]
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
struct KillFlags {
    /// Also end every descendant (race-free subtree kill).
    #[arg(long)]
    tree: bool,
    /// End every process started from the target's terminal window (keeps the shell).
    #[arg(long)]
    console: bool,
    /// Skip the graceful request and terminate right away.
    #[arg(long)]
    force: bool,
    /// How long the graceful request may take before forcing (e.g. 5s, 500ms).
    #[arg(long, default_value = "5s", value_parser = parse_duration)]
    grace: Duration,
    /// Do not ask for confirmation.
    #[arg(long, short = 'y')]
    yes: bool,
    /// Show what would happen; change nothing.
    #[arg(long)]
    dry_run: bool,
}

impl From<KillFlags> for KillArgs {
    fn from(f: KillFlags) -> Self {
        KillArgs {
            tree: f.tree,
            console: f.console,
            force: f.force,
            grace: f.grace,
            yes: f.yes,
            dry_run: f.dry_run,
        }
    }
}

#[derive(Subcommand, Debug)]
enum Cmd {
    /// Which process owns a port (exit 0 found or done, 1 none, 2 denied, 3 stuck).
    Port {
        port: u16,
        /// End the owner(s).
        #[arg(long)]
        kill: bool,
        #[command(flatten)]
        k: KillFlags,
    },
    /// Search processes: text, pid:123, user:me, name:node, :3000 (exit 0 found, 1 none).
    Find {
        #[arg(required = true, num_args = 1..)]
        query: Vec<String>,
    },
    /// End a process by PID or search (exit 0 done, 2 denied/protected, 3 stuck).
    Kill {
        target: String,
        #[command(flatten)]
        k: KillFlags,
    },
    /// Write every process (tree, statics, labels, ports) as JSON, for bug reports. Secret-looking
    /// command-line values are masked unless --unmasked.
    Export {
        /// Write to this file instead of stdout.
        #[arg(long)]
        out: Option<std::path::PathBuf>,
        /// Keep command lines as they are (they may contain passwords or tokens).
        #[arg(long)]
        unmasked: bool,
    },
    /// What has a file, or files inside a folder, open (exit 0 found, 1 none, 2 missing/failed).
    Lock { path: std::path::PathBuf },
    /// Show the process tree, or the subtree of one PID.
    Tree { pid: Option<u32> },
    /// Wait until nothing uses a port (exit 0 free, 4 timeout).
    WaitPort {
        port: u16,
        #[arg(long, default_value = "30s", value_parser = parse_duration)]
        timeout: Duration,
    },
}

/// `[y/N]` on an interactive terminal; never blocks a script (no terminal = no).
fn confirm(prompt: &str) -> bool {
    if !std::io::stdin().is_terminal() {
        eprintln!("{prompt} Not confirmed: pass --yes to run without a prompt.");
        return false;
    }
    eprint!("{prompt} [y/N] ");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    matches!(line.trim(), "y" | "Y" | "yes" | "YES" | "Yes")
}

fn main() -> ExitCode {
    // Internal console helper for the kill engine: `__ctrl-c` / `__console-list <pid> <ct>`.
    #[cfg(windows)]
    if matches!(
        std::env::args().nth(1).as_deref(),
        Some("__ctrl-c" | "__console-list")
    ) {
        return atropos_core::platform::windows::ctrl_c_helper_main(std::env::args().skip(1));
    }
    let cli = Cli::parse();
    let Some(cmd) = cli.cmd else {
        let result = std::env::current_exe()
            .map_err(anyhow::Error::from)
            .and_then(|exe| launch::launch_gui(&exe));
        return match result {
            Ok(()) => ExitCode::SUCCESS,
            Err(e) => {
                eprintln!("atr: {e}");
                ExitCode::from(1)
            }
        };
    };
    let me = match atropos_core::platform::current_process_key() {
        Ok(k) => k,
        Err(e) => {
            eprintln!("atr: cannot read own process identity: {e}");
            return ExitCode::from(2);
        }
    };
    let mut platform = atropos_core::platform::Native::new();
    #[cfg(windows)]
    if let Ok(exe) = std::env::current_exe() {
        // atr.exe is itself the console Ctrl+C helper.
        platform = platform.with_ctrl_c_helper(exe);
    }
    let mut stdout = std::io::stdout().lock();
    let mut ask = confirm;
    let mut env = Env {
        platform: &mut platform,
        me,
        out: &mut stdout,
        json: cli.json,
        confirm: &mut ask,
    };
    let code = match cmd {
        Cmd::Port { port, kill, k } => {
            let args = (kill || k.dry_run).then(|| KillArgs::from(k));
            run::port(&mut env, port, args)
        }
        Cmd::Find { query } => run::find(&mut env, &query.join(" ")),
        Cmd::Kill { target, k } => run::kill_cmd(&mut env, &target, k.into()),
        Cmd::Tree { pid } => run::tree(&mut env, pid),
        Cmd::Lock { path } => run::lock(&mut env, &path),
        Cmd::Export { out, unmasked } => run::export(&mut env, out.as_deref(), unmasked),
        Cmd::WaitPort { port, timeout } => run::wait_port(&mut env, port, timeout),
    };
    ExitCode::from(code)
}
