//! Fixture: prints its key line, then spawns a `sleeper` child every 100 ms (at most `--max`,
//! default 40) on its own console, each living `--secs` (default 60). Used to prove a subtree
//! kill leaves no survivors even while the tree keeps growing.

use std::io::Write as _;
use std::time::Duration;

fn arg(name: &str, default: u64) -> u64 {
    std::env::args()
        .skip_while(|a| a != name)
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(default)
}

fn main() -> anyhow::Result<()> {
    let (max, secs) = (arg("--max", 40), arg("--secs", 60));
    let _ = atropos_core::platform::windows::fixture_support::default_ctrl_c();
    let key = atropos_core::platform::windows::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    out.flush()?;
    drop(out);
    let sleeper = std::env::current_exe()?.with_file_name("sleeper.exe");
    let mut children = Vec::new();
    for _ in 0..max {
        if let Ok(c) = std::process::Command::new(&sleeper)
            .args(["--secs", &secs.to_string()])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .spawn()
        {
            children.push(c);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
