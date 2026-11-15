//! "What is locking this file or folder?" (spec Phase 6), the OS-independent part: which files
//! to ask about, and a time bound around the OS call (Restart Manager may wait on a network
//! share; the lookup must never hang the caller).

use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::{CoreError, Result};

/// The most files a folder lookup registers (a huge tree is cut off, not walked for minutes).
pub const MAX_FILES: usize = 2000;

/// How long a lookup may take before the caller gets [`CoreError::TimedOut`].
pub const TIMEOUT: Duration = Duration::from_secs(10);

/// The files to register for `path`: the file itself, or the files inside a folder
/// (recursively, at most `cap`, links and junctions not followed).
///
/// # Errors
/// [`CoreError::NotFound`] if `path` does not exist.
pub fn targets(path: &Path, cap: usize) -> Result<Vec<PathBuf>> {
    let meta = std::fs::metadata(path).map_err(|_| CoreError::NotFound)?;
    if !meta.is_dir() {
        return Ok(vec![path.to_path_buf()]);
    }
    let mut out = Vec::new();
    let mut dirs = vec![path.to_path_buf()];
    while let Some(d) = dirs.pop() {
        // An unreadable subfolder is skipped; the rest is still asked about.
        let Ok(entries) = std::fs::read_dir(&d) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if kind.is_dir() {
                dirs.push(entry.path());
            } else if kind.is_file() {
                if out.len() >= cap {
                    return Ok(out);
                }
                out.push(entry.path());
            }
        }
    }
    Ok(out)
}

/// Runs `f` on its own thread and waits at most `timeout`. A call that overruns keeps running
/// in the background and its result is dropped.
///
/// # Errors
/// [`CoreError::TimedOut`] after `timeout`; otherwise whatever `f` returns.
pub fn bounded<T: Send + 'static>(
    timeout: Duration,
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::Builder::new()
        .name("atropos-bounded".into())
        .spawn(move || {
            let _ = tx.send(f());
        })
        .map_err(|_| CoreError::Unsupported)?;
    rx.recv_timeout(timeout).unwrap_or(Err(CoreError::TimedOut))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atropos-locks-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(d.join("sub")).unwrap();
        d
    }

    #[test]
    fn a_file_is_its_own_target() {
        let d = dir("file");
        let f = d.join("a.txt");
        std::fs::write(&f, "x").unwrap();
        assert_eq!(targets(&f, MAX_FILES).unwrap(), vec![f]);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_folder_gives_the_files_inside_capped() {
        let d = dir("folder");
        for f in ["b.txt", "a.txt", "sub/c.txt"] {
            std::fs::write(d.join(f), "x").unwrap();
        }
        let mut all = targets(&d, MAX_FILES).unwrap();
        all.sort();
        assert_eq!(
            all,
            vec![
                d.join("a.txt"),
                d.join("b.txt"),
                d.join("sub").join("c.txt")
            ]
        );
        assert_eq!(targets(&d, 2).unwrap().len(), 2);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn an_empty_folder_has_no_targets() {
        let d = dir("empty");
        assert_eq!(targets(&d, MAX_FILES).unwrap(), Vec::<PathBuf>::new());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_missing_path_is_not_found() {
        let d = dir("missing");
        assert_eq!(
            targets(&d.join("nope"), MAX_FILES),
            Err(CoreError::NotFound)
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn a_slow_call_times_out_and_a_quick_one_answers() {
        let slow = bounded(Duration::from_millis(50), || {
            std::thread::sleep(Duration::from_secs(2));
            Ok(1)
        });
        assert_eq!(slow, Err(CoreError::TimedOut));
        assert_eq!(bounded(Duration::from_secs(5), || Ok(7)), Ok(7));
        assert_eq!(
            bounded::<u8>(Duration::from_secs(5), || Err(CoreError::Unsupported)),
            Err(CoreError::Unsupported)
        );
    }
}
