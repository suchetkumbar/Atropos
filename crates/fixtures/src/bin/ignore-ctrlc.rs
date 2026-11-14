//! Fixture: a console app that ignores Ctrl+C. Prints its key line, then sleeps (`--secs`).

use std::io::Write as _;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let secs = std::env::args()
        .skip_while(|a| a != "--secs")
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(3600);
    atropos_core::platform::windows::fixture_support::ignore_ctrl_c()?;
    let key = atropos_core::platform::windows::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    out.flush()?;
    drop(out);
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
