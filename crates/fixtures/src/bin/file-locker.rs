//! Fixture: creates (or opens) the file given by `--path <p>`, keeps it open, prints its key
//! line, then `locked`, and sleeps (`--secs <n>`, default 3600).

use std::io::Write as _;
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let (mut path, mut secs) = (None, 3600u64);
    let mut args = std::env::args_os().skip(1);
    while let Some(a) = args.next() {
        match (a.to_str(), args.next()) {
            (Some("--path"), Some(p)) => path = Some(std::path::PathBuf::from(p)),
            (Some("--secs"), Some(s)) => {
                secs = s.to_str().and_then(|s| s.parse().ok()).unwrap_or(secs);
            }
            _ => {}
        }
    }
    let path = path.ok_or_else(|| anyhow::anyhow!("--path is required"))?;
    // Held for the whole sleep.
    let _file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    let key = atropos_core::platform::windows::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    writeln!(out, "locked")?;
    out.flush()?;
    drop(out);
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
