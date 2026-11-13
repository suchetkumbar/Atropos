//! Test-only fixtures. [`spawn`] starts a fixture exe and returns its [`ProcKey`], read from the
//! fixture's first stdout line. Tests act **only** on keys obtained this way (safety rule).

use std::io::{BufRead as _, BufReader};
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use anyhow::Context as _;
use atropos_core::ProcKey;

/// How long a fixture may take to print its key line.
const READY_TIMEOUT: Duration = Duration::from_secs(5);

/// A running fixture. Dropping it kills and reaps the child through its own handle.
#[derive(Debug)]
pub struct SpawnedFixture {
    pub key: ProcKey,
    child: Child,
}

impl SpawnedFixture {
    /// The child's PID, for assertions only; never pass it to an action.
    #[must_use]
    pub fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for SpawnedFixture {
    fn drop(&mut self) {
        // Acts on the handle of a process this harness spawned, so PID reuse cannot redirect it.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// Path of a built fixture exe: the first `<name>.exe` found in the running test's folder or any
/// ancestor folder (works with both the classic `deps/` layout and Cargo's newer `build-dir` layout).
///
/// # Errors
/// If the current exe path is unknown or the fixture has not been built.
pub fn fixture_path(name: &str) -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe().context("current_exe")?;
    let file = format!("{name}.exe");
    exe.ancestors()
        .skip(1)
        .map(|dir| dir.join(&file))
        .find(|p| p.is_file())
        .with_context(|| {
            format!("fixture '{name}' not built: run cargo build -p atropos-fixtures (searched above {})", exe.display())
        })
}

/// Spawns fixture `name` with `args` and waits for its key line.
///
/// # Errors
/// If the fixture is missing, fails to start, or does not report its key within 5 s.
pub fn spawn(name: &str, args: &[&str]) -> anyhow::Result<SpawnedFixture> {
    let path = fixture_path(name)?;
    let mut child = Command::new(&path)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .with_context(|| format!("spawn {}", path.display()))?;
    let stdout = child.stdout.take().context("no stdout pipe")?;
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        let mut line = String::new();
        let _ = BufReader::new(stdout).read_line(&mut line);
        let _ = tx.send(line);
    });
    let mut fixture = SpawnedFixture {
        key: ProcKey {
            pid: child.id(),
            create_time: 0,
        },
        child,
    };
    let line = rx.recv_timeout(READY_TIMEOUT).with_context(|| {
        format!("fixture '{name}' did not report its key within {READY_TIMEOUT:?}")
    })?;
    fixture.key =
        parse_key_line(&line).with_context(|| format!("fixture '{name}' printed {line:?}"))?;
    Ok(fixture)
}

/// Formats the key line every fixture prints first: `{"pid":<u32>,"create_time":<u64>}`.
#[must_use]
pub fn key_line(key: ProcKey) -> String {
    format!(
        "{{\"pid\":{},\"create_time\":{}}}",
        key.pid, key.create_time
    )
}

/// Parses a [`key_line`].
///
/// # Errors
/// If the line is not in that exact shape.
pub fn parse_key_line(line: &str) -> anyhow::Result<ProcKey> {
    let body = line
        .trim()
        .strip_prefix("{\"pid\":")
        .context("missing pid")?;
    let (pid, rest) = body
        .split_once(",\"create_time\":")
        .context("missing create_time")?;
    let create_time = rest.strip_suffix('}').context("missing closing brace")?;
    Ok(ProcKey {
        pid: pid.parse()?,
        create_time: create_time.parse()?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_line_round_trips() {
        let k = ProcKey {
            pid: 4242,
            create_time: 133_000_000_000_000_000,
        };
        assert_eq!(parse_key_line(&format!("{}\r\n", key_line(k))).unwrap(), k);
    }

    #[test]
    fn malformed_key_line_is_rejected() {
        assert!(parse_key_line("hello").is_err());
        assert!(parse_key_line("{\"pid\":1}").is_err());
    }
}
