//! Kill history and Restart (spec Phase 6): one JSON line per process Atropos ended, in
//! `%LOCALAPPDATA%\Atropos\history.jsonl`, with the launch record (exe, command line, cwd,
//! environment) captured *before* the kill, so **Restart** can start it again the same way.
//!
//! The file lives in the user's own profile folder (readable by that account, SYSTEM and
//! administrators, like everything there). Environment values are stored as they were, because
//! Restart needs them; the UI never shows them. The file is append-only, bad lines are skipped,
//! and past [`MAX_BYTES`] it is rotated to `history.1.jsonl` (one old generation is kept).

use std::io::Write as _;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::platform::{Platform, ProcEnv};
use crate::statics::Statics;
use crate::{Field, ProcKey};

/// Rotate once the file reaches this size.
pub const MAX_BYTES: u64 = 1024 * 1024;
pub const VERSION: u8 = 1;

/// How the process was started: enough to start it again.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Launch {
    pub exe: String,
    /// The command line exactly as the process received it (passed back verbatim, so no
    /// re-quoting can change an argument).
    pub cmdline: String,
    pub cwd: String,
    /// In block order; `=C:`-style drive entries are not included.
    pub env: Vec<(String, String)>,
    /// Size and modification time of `exe` when captured: a changed exe is not restarted.
    pub exe_len: u64,
    pub exe_modified: u64,
}

/// One ended process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Entry {
    pub v: u8,
    /// When it was ended: FILETIME ticks (100 ns since 1601).
    pub at: u64,
    pub pid: u32,
    pub create_time: u64,
    pub name: String,
    /// Ended with its subtree.
    pub tree: bool,
    /// `exited` (closed when asked) or `terminated` (forced).
    pub outcome: String,
    /// `None` when the command line, cwd or environment could not be read (no Restart).
    pub launch: Option<Launch>,
}

impl Entry {
    #[must_use]
    pub fn key(&self) -> ProcKey {
        ProcKey {
            pid: self.pid,
            create_time: self.create_time,
        }
    }
}

/// `%LOCALAPPDATA%\Atropos\history.jsonl` (next to the logs folder).
#[must_use]
pub fn path() -> PathBuf {
    crate::paths::log_dir()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("history.jsonl")
}

/// The rotated generation of `path`.
#[must_use]
pub fn rotated(path: &Path) -> PathBuf {
    path.with_extension("1.jsonl")
}

/// Size and modification time (nanoseconds since the Unix epoch; only equality matters).
fn stamp(exe: &Path) -> Option<(u64, u64)> {
    let m = std::fs::metadata(exe).ok()?;
    let modified = m
        .modified()
        .ok()?
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?;
    Some((
        m.len(),
        u64::try_from(modified.as_nanos()).unwrap_or(u64::MAX),
    ))
}

/// The launch record of a live process, from its statics and its PEB (tier 2).
#[must_use]
pub fn capture(statics: &Statics, env: &Field<ProcEnv>) -> Option<Launch> {
    let exe = statics.exe_path.ok()?;
    let cmdline = statics.cmdline.ok()?;
    let env = env.ok()?;
    let (exe_len, exe_modified) = stamp(Path::new(&**exe))?;
    Some(Launch {
        exe: exe.to_string(),
        cmdline: cmdline.to_string(),
        cwd: env.cwd.to_string(),
        env: env.vars.clone(),
        exe_len,
        exe_modified,
    })
}

/// Appends `e`, rotating first when the file has reached `max` bytes.
///
/// # Errors
/// If the folder or file cannot be written (e.g. locked by another program).
pub fn append(path: &Path, e: &Entry, max: u64) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    if std::fs::metadata(path).is_ok_and(|m| m.len() >= max) {
        std::fs::rename(path, rotated(path))?;
    }
    let mut line = serde_json::to_vec(e).map_err(std::io::Error::other)?;
    line.push(b'\n');
    // One write per line: appends from two Atropos windows interleave by whole lines.
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)?
        .write_all(&line)
}

/// Every readable entry, oldest first (the rotated generation, then the current file).
/// Unreadable files and bad lines are skipped.
#[must_use]
pub fn read(path: &Path) -> Vec<Entry> {
    [rotated(path), path.to_path_buf()]
        .iter()
        .filter_map(|p| std::fs::read(p).ok())
        .flat_map(|bytes| {
            bytes
                .split(|b| *b == b'\n')
                .filter_map(|line| serde_json::from_slice::<Entry>(line).ok())
                .collect::<Vec<_>>()
        })
        .collect()
}

/// Starts `l` again. Refuses, with a message for the user, when the program or folder is
/// gone, the program changed since it was captured, or Atropos runs elevated (a restart must
/// never gain rights the original may not have had).
///
/// # Errors
/// The message to show; nothing was started.
pub fn restart(platform: &mut dyn Platform, l: &Launch, elevated: bool) -> Result<ProcKey, String> {
    let exe = Path::new(&l.exe);
    let Some(now) = stamp(exe) else {
        return Err(format!("{} no longer exists; nothing was started.", l.exe));
    };
    if now != (l.exe_len, l.exe_modified) {
        return Err(format!(
            "{} has changed since it was ended (updated?); nothing was started. Start it \
             yourself to be sure it runs the way you expect.",
            l.exe
        ));
    }
    if !Path::new(&l.cwd).is_dir() {
        return Err(format!(
            "The folder {} no longer exists; nothing was started.",
            l.cwd
        ));
    }
    if elevated {
        return Err(
            "Atropos is running as administrator, so Restart could give the program more \
             rights than it had; nothing was started. Restart it from a normal Atropos."
                .into(),
        );
    }
    platform
        .launch(l)
        .map_err(|e| format!("Windows refused to start it ({e})."))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::FakePlatform;
    use std::sync::Arc;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atropos-hist {tag} ü {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn launch(d: &Path) -> Launch {
        let exe = d.join("tool.exe");
        std::fs::write(&exe, "MZ").unwrap();
        let (exe_len, exe_modified) = stamp(&exe).unwrap();
        Launch {
            exe: exe.display().to_string(),
            cmdline: format!(r#""{}" --port 3000 "a b""#, exe.display()),
            cwd: d.display().to_string(),
            env: vec![
                ("PATH".into(), r"C:\bin".into()),
                ("API_TOKEN".into(), "s3cr3t".into()),
            ],
            exe_len,
            exe_modified,
        }
    }

    fn entry(pid: u32, launch: Option<Launch>) -> Entry {
        Entry {
            v: VERSION,
            at: 133_000_000_000_000_000,
            pid,
            create_time: u64::from(pid) * 10,
            name: "node.exe".into(),
            tree: false,
            outcome: "exited".into(),
            launch,
        }
    }

    #[test]
    fn entries_round_trip_through_the_file_in_order() {
        let d = dir("roundtrip");
        let p = d.join("history.jsonl");
        let a = entry(1, Some(launch(&d)));
        let b = entry(2, None);
        append(&p, &a, MAX_BYTES).unwrap();
        append(&p, &b, MAX_BYTES).unwrap();
        assert_eq!(read(&p), vec![a, b]);
        let text = std::fs::read_to_string(&p).unwrap();
        assert_eq!(text.lines().count(), 2, "one JSON object per line");
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn bad_lines_are_skipped() {
        let d = dir("bad");
        let p = d.join("history.jsonl");
        append(&p, &entry(1, None), MAX_BYTES).unwrap();
        let mut f = std::fs::OpenOptions::new().append(true).open(&p).unwrap();
        writeln!(f, "{{not json").unwrap();
        writeln!(f, r#"{{"v":1,"half":true}}"#).unwrap();
        f.write_all(b"\xff\xfe garbage\n").unwrap();
        drop(f);
        append(&p, &entry(2, None), MAX_BYTES).unwrap();
        let pids: Vec<u32> = read(&p).iter().map(|e| e.pid).collect();
        assert_eq!(pids, vec![1, 2]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_full_file_is_rotated_and_one_generation_kept() {
        let d = dir("rotate");
        let p = d.join("history.jsonl");
        let line = serde_json::to_string(&entry(1, None)).unwrap().len() as u64 + 1;
        // Two lines fit; the third append rotates.
        for pid in 1..=5 {
            append(&p, &entry(pid, None), line * 2).unwrap();
        }
        let pids: Vec<u32> = read(&p).iter().map(|e| e.pid).collect();
        assert_eq!(pids, vec![3, 4, 5], "older generations are dropped");
        assert!(rotated(&p).exists());
        assert!(std::fs::metadata(&p).unwrap().len() <= line * 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_file_reads_as_empty() {
        let d = dir("missing");
        assert_eq!(read(&d.join("history.jsonl")), Vec::new());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn capture_needs_exe_cmdline_and_environment() {
        let d = dir("capture");
        let exe = d.join("tool.exe");
        std::fs::write(&exe, "MZ").unwrap();
        let statics = Statics {
            exe_path: Field::Ok(Arc::from(exe.display().to_string())),
            cmdline: Field::Ok(Arc::from("tool.exe --x")),
            ..Statics::unavailable(&Field::Restricted)
        };
        let env = ProcEnv {
            cwd: Arc::from(r"D:\code\shop\"),
            vars: vec![("A".into(), "1".into())],
        };
        let l = capture(&statics, &Field::Ok(env.clone())).unwrap();
        assert_eq!(l.exe, exe.display().to_string());
        assert_eq!(l.cmdline, "tool.exe --x");
        assert_eq!(l.cwd, r"D:\code\shop\");
        assert_eq!(l.env, vec![("A".to_owned(), "1".to_owned())]);
        assert_eq!(l.exe_len, 2);
        assert_eq!(capture(&statics, &Field::Restricted), None);
        let no_cmd = Statics {
            cmdline: Field::Restricted,
            ..statics
        };
        assert_eq!(capture(&no_cmd, &Field::Ok(env)), None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restart_launches_the_record_as_captured() {
        let d = dir("restart");
        let l = launch(&d);
        let mut f = FakePlatform::default();
        let key = restart(&mut f, &l, false).unwrap();
        assert_eq!(f.launched, vec![l]);
        assert_eq!(key.pid, 1000);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn restart_refuses_a_missing_or_changed_program_a_missing_folder_or_elevation() {
        let d = dir("refuse");
        let l = launch(&d);
        let mut f = FakePlatform::default();

        let gone_cwd = Launch {
            cwd: d.join("deleted").display().to_string(),
            ..l.clone()
        };
        assert!(
            restart(&mut f, &gone_cwd, false)
                .unwrap_err()
                .contains("folder")
        );

        assert!(
            restart(&mut f, &l, true)
                .unwrap_err()
                .contains("administrator")
        );

        std::fs::write(&l.exe, "MZ updated").unwrap();
        assert!(restart(&mut f, &l, false).unwrap_err().contains("changed"));

        std::fs::remove_file(&l.exe).unwrap();
        assert!(
            restart(&mut f, &l, false)
                .unwrap_err()
                .contains("no longer exists")
        );

        assert_eq!(f.launched, Vec::new(), "nothing was started");
        let _ = std::fs::remove_dir_all(&d);
    }
}
