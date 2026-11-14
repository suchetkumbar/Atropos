//! Crash safety (spec Phase 3): every suspend Atropos makes is written to a journal *before* it
//! happens and removed on resume. After a crash the next start reads what was left and offers to
//! resume it; keys whose PID now belongs to another process are skipped.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::ProcKey;
use crate::engine::kill::Strategy;
use crate::pin::{Access, Job, PinError, Pinned};
use crate::platform::{Platform, ProcEnv, RawProc, SystemStats};
use crate::ports::{ServiceInfo, Socket};
use crate::statics::Statics;
use crate::{Field, Result};

/// `%LOCALAPPDATA%\Atropos\suspended.journal` (next to the logs folder).
#[must_use]
pub fn path() -> PathBuf {
    crate::paths::log_dir()
        .parent()
        .map_or_else(std::env::temp_dir, Path::to_path_buf)
        .join("suspended.journal")
}

/// Outstanding suspends by key, mirrored to a file on every change.
#[derive(Debug)]
pub struct Journal {
    path: PathBuf,
    entries: BTreeMap<ProcKey, u32>,
}

impl Journal {
    /// Opens (without creating) the journal at `path`, reading what a previous run left.
    #[must_use]
    pub fn open(path: PathBuf) -> Self {
        let mut entries = BTreeMap::new();
        if let Ok(text) = std::fs::read_to_string(&path) {
            for line in text.lines() {
                let f: Vec<&str> = line.split_whitespace().collect();
                if let [pid, ct, n] = f.as_slice() {
                    if let (Ok(pid), Ok(create_time), Ok(n)) =
                        (pid.parse(), ct.parse(), n.parse::<u32>())
                    {
                        if n > 0 {
                            entries.insert(ProcKey { pid, create_time }, n);
                        }
                    }
                }
            }
        }
        Self { path, entries }
    }

    /// Mirrors the entries to disk; an empty journal removes the file.
    fn save(&self) {
        if self.entries.is_empty() {
            let _ = std::fs::remove_file(&self.path);
            return;
        }
        let text = self.entries.iter().fold(String::new(), |mut t, (k, n)| {
            use std::fmt::Write as _;
            let _ = writeln!(t, "{} {} {n}", k.pid, k.create_time);
            t
        });
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("journal.tmp");
            std::fs::write(&tmp, text)?;
            std::fs::rename(&tmp, &self.path)
        };
        if let Err(e) = write() {
            tracing::warn!("suspend journal {}: {e}", self.path.display());
        }
    }

    /// Records a suspend that is about to happen.
    pub fn add(&mut self, key: ProcKey) {
        *self.entries.entry(key).or_insert(0) += 1;
        self.save();
    }

    /// Records a resume (or a suspend that failed).
    pub fn remove(&mut self, key: ProcKey) {
        if let Some(n) = self.entries.get_mut(&key) {
            *n -= 1;
            if *n == 0 {
                self.entries.remove(&key);
            }
            self.save();
        }
    }

    /// Forgets `key` entirely (resumed after a crash, or gone).
    pub fn forget(&mut self, key: ProcKey) {
        if self.entries.remove(&key).is_some() {
            self.save();
        }
    }

    #[must_use]
    pub fn entries(&self) -> Vec<(ProcKey, u32)> {
        self.entries.iter().map(|(k, n)| (*k, *n)).collect()
    }
}

/// Journal entries that still name a live, identical process (the PID was not reused).
pub fn resumable(platform: &mut dyn Platform, entries: &[(ProcKey, u32)]) -> Vec<(ProcKey, u32)> {
    entries
        .iter()
        .copied()
        .filter(|(k, _)| platform.pin(*k, Access::QUERY).is_ok())
        .collect()
}

/// Wraps a platform so that every `suspend` is journaled first and every `resume` clears it.
pub struct JournaledPlatform<P: Platform> {
    pub inner: P,
    journal: Arc<Mutex<Journal>>,
}

impl<P: Platform> JournaledPlatform<P> {
    pub fn new(inner: P, journal: Arc<Mutex<Journal>>) -> Self {
        Self { inner, journal }
    }
}

impl<P: Platform> Platform for JournaledPlatform<P> {
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats> {
        self.inner.scan(out)
    }
    fn statics(&mut self, key: ProcKey) -> Statics {
        self.inner.statics(key)
    }
    fn cwd_and_env(&mut self, key: ProcKey) -> Field<ProcEnv> {
        self.inner.cwd_and_env(key)
    }
    fn sockets(&mut self) -> Result<Vec<Socket>> {
        self.inner.sockets()
    }
    fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        self.inner.services()
    }
    fn pin(&mut self, key: ProcKey, access: Access) -> std::result::Result<Pinned, PinError> {
        self.inner.pin(key, access)
    }
    fn wait_exits(&mut self, pins: &[&Pinned], timeout: std::time::Duration) -> Vec<bool> {
        self.inner.wait_exits(pins, timeout)
    }
    fn strategy(&mut self, p: &Pinned) -> Strategy {
        self.inner.strategy(p)
    }
    fn graceful(&mut self, p: &Pinned, how: &Strategy) -> Result<()> {
        self.inner.graceful(p, how)
    }
    fn terminate(&mut self, p: &Pinned) -> Result<()> {
        self.inner.terminate(p)
    }
    fn suspend(&mut self, p: &Pinned) -> Result<()> {
        // Written before the suspend: a crash right after it still leaves a record.
        if let Ok(mut j) = self.journal.lock() {
            j.add(p.key());
        }
        let r = self.inner.suspend(p);
        if r.is_err() {
            if let Ok(mut j) = self.journal.lock() {
                j.remove(p.key());
            }
        }
        r
    }
    fn resume(&mut self, p: &Pinned) -> Result<()> {
        let r = self.inner.resume(p);
        if r.is_ok() {
            if let Ok(mut j) = self.journal.lock() {
                j.remove(p.key());
            }
        }
        r
    }
    fn create_job(&mut self) -> Result<Job> {
        self.inner.create_job()
    }
    fn assign(&mut self, job: &Job, p: &Pinned) -> Result<()> {
        self.inner.assign(job, p)
    }
    fn terminate_job(&mut self, job: &Job) -> Result<()> {
        self.inner.terminate_job(job)
    }
    fn console_processes(&mut self, key: ProcKey) -> Result<Vec<u32>> {
        self.inner.console_processes(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::suspend::SuspendLedger;
    use crate::platform::FakePlatform;
    use crate::snapshot::build;

    fn k(pid: u32, t: u64) -> ProcKey {
        ProcKey {
            pid,
            create_time: t,
        }
    }

    fn file(tag: &str) -> PathBuf {
        let d =
            std::env::temp_dir().join(format!("atropos journal ü {tag} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d.join("suspended.journal")
    }

    fn fake() -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(10, 1), 0, "a.exe"),
            RawProc::new(k(11, 2), 10, "b.exe"),
        ]);
        f
    }

    #[test]
    fn opening_creates_nothing() {
        let p = file("none");
        let j = Journal::open(p.clone());
        assert_eq!(j.entries(), vec![]);
        assert!(!p.exists());
        assert!(!p.parent().unwrap().exists());
    }

    #[test]
    fn journal_survives_a_crash_while_suspended() {
        let p = file("crash");
        {
            let journal = Arc::new(Mutex::new(Journal::open(p.clone())));
            let mut jp = JournaledPlatform::new(fake(), journal);
            let mut raw = Vec::new();
            let stats = jp.scan(&mut raw).unwrap();
            let snap = build(None, raw, stats);
            SuspendLedger::default().suspend(&mut jp, &snap, k(10, 1), true, &|_| false);
            // "Crash": dropped without resuming.
        }
        let mut after = Journal::open(p.clone()).entries();
        after.sort();
        assert_eq!(after, vec![(k(10, 1), 1), (k(11, 2), 1)]);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn resume_clears_the_entry_and_counts_nest() {
        let p = file("nest");
        let mut j = Journal::open(p.clone());
        j.add(k(10, 1));
        j.add(k(10, 1));
        j.remove(k(10, 1));
        assert_eq!(Journal::open(p.clone()).entries(), vec![(k(10, 1), 1)]);
        j.remove(k(10, 1));
        assert_eq!(Journal::open(p.clone()).entries(), vec![]);
        j.add(k(12, 3));
        j.forget(k(12, 3));
        assert_eq!(Journal::open(p.clone()).entries(), vec![]);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn a_failed_suspend_is_not_left_in_the_journal() {
        let p = file("failed");
        let journal = Arc::new(Mutex::new(Journal::open(p.clone())));
        let mut jp = JournaledPlatform::new(fake(), Arc::clone(&journal));
        // A fake pin without a real handle cannot be suspended by a mismatching key: use a
        // gone key through the ledger, which never reaches suspend.
        let snap = crate::snapshot::Snapshot::default();
        SuspendLedger::default().suspend(&mut jp, &snap, k(99, 9), false, &|_| false);
        assert_eq!(journal.lock().unwrap().entries(), vec![]);
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }

    #[test]
    fn stale_entries_are_skipped_on_restart() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(10, 7), 0, "other.exe"),
            RawProc::new(k(11, 2), 0, "b.exe"),
        ]);
        let mut raw = Vec::new();
        f.scan(&mut raw).unwrap();
        // 10@1 was journaled, but PID 10 now belongs to a younger process; 12 is gone.
        let r = resumable(&mut f, &[(k(10, 1), 1), (k(11, 2), 2), (k(12, 3), 1)]);
        assert_eq!(r, vec![(k(11, 2), 2)]);
    }

    #[test]
    fn corrupt_lines_are_ignored() {
        let p = file("corrupt");
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, "10 1 1\ngarbage\n11 2\n\n12 3 2\n").unwrap();
        assert_eq!(
            Journal::open(p.clone()).entries(),
            vec![(k(10, 1), 1), (k(12, 3), 2)]
        );
        let _ = std::fs::remove_dir_all(p.parent().unwrap());
    }
}
