//! Fixture: a GUI-subsystem app with one visible top-level window (placed off-screen) that exits
//! when it receives `WM_CLOSE`. Prints its key line first; exits anyway after `--secs`.
#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(windows)]
use std::io::Write as _;

#[cfg(windows)]
fn main() -> anyhow::Result<()> {
    let secs = std::env::args()
        .skip_while(|a| a != "--secs")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    let key = atropos_core::platform::windows::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    out.flush()?;
    drop(out);
    atropos_core::platform::windows::fixture_support::run_window("atropos-test-window", secs)?;
    Ok(())
}

/// Windows only.
#[cfg(not(windows))]
fn main() {}
