//! Fixture: prints its own key line, then sleeps (`--secs <n>`, default 3600) and exits 0.
//! Default Ctrl+C handling: Ctrl+C ends it.

use std::io::Write as _;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let mut secs: u64 = 3600;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--secs" {
            secs = args.next().and_then(|v| v.parse().ok()).unwrap_or(secs);
        }
    }
    // Behave like a process started from a terminal, whatever the test runner inherited.
    // (No console at all, e.g. a detached start, is fine: then nothing to restore.)
    #[cfg(windows)]
    let _ = atropos_core::platform::windows::fixture_support::default_ctrl_c();
    let key = atropos_core::platform::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    out.flush()?;
    drop(out);
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
