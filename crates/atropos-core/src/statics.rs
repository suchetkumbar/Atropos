//! Tier-1 statics (spec §3.3): per-process facts that never change while the process lives.
//! Fetched once per [`ProcKey`] and evicted when the key leaves the snapshot.

use std::collections::HashMap;
use std::sync::Arc;

use crate::platform::Platform;
use crate::snapshot::Snapshot;
use crate::{Field, ProcKey};

/// Code-integrity protection level of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protection {
    None,
    /// Protected Process Light (anti-malware services, `csrss.exe`, …).
    Light,
    /// Fully protected process.
    Full,
}

/// Facts read once per process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Statics {
    pub exe_path: Field<Arc<str>>,
    pub cmdline: Field<Arc<str>>,
    /// Account name (no domain), as Task Manager shows it.
    pub user: Field<Arc<str>>,
    /// A 32-bit process on a 64-bit OS.
    pub wow64: Field<bool>,
    pub protection: Field<Protection>,
}

impl Statics {
    /// Every field set to the same unavailable reason (`Field::Ok` is treated as `Unsupported`).
    #[must_use]
    pub fn unavailable(reason: &Field<()>) -> Self {
        Self {
            exe_path: reason_only(reason),
            cmdline: reason_only(reason),
            user: reason_only(reason),
            wow64: reason_only(reason),
            protection: reason_only(reason),
        }
    }
}

fn reason_only<T>(reason: &Field<()>) -> Field<T> {
    match reason {
        Field::Restricted => Field::Restricted,
        Field::Gone => Field::Gone,
        Field::Ok(()) | Field::Unsupported => Field::Unsupported,
    }
}

/// Statics by key, shared with the UI as one immutable map per frame.
pub type StaticsMap = HashMap<ProcKey, Arc<Statics>>;

/// Keeps [`Statics`] for exactly the keys in the latest snapshot.
#[derive(Debug, Default)]
pub struct StaticsCache {
    map: StaticsMap,
}

impl StaticsCache {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Fetches statics for keys new in `snap` and drops keys no longer in it.
    /// Returns whether the set of keys changed.
    pub fn refresh(&mut self, platform: &mut dyn Platform, snap: &Snapshot) -> bool {
        let before = self.map.len();
        self.map.retain(|k, _| snap.index.contains_key(k));
        let mut changed = self.map.len() != before;
        for p in &snap.procs {
            let key = p.raw.key;
            self.map.entry(key).or_insert_with(|| {
                changed = true;
                Arc::new(platform.statics(key))
            });
        }
        changed
    }

    #[must_use]
    pub fn get(&self, key: ProcKey) -> Option<&Arc<Statics>> {
        self.map.get(&key)
    }

    #[must_use]
    pub fn map(&self) -> &StaticsMap {
        &self.map
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc};
    use crate::snapshot::build;

    fn k(pid: u32, t: u64) -> ProcKey {
        ProcKey {
            pid,
            create_time: t,
        }
    }

    fn tick(f: &mut FakePlatform, cache: &mut StaticsCache) -> Snapshot {
        let mut out = Vec::new();
        let stats = f.scan(&mut out).unwrap();
        let snap = build(None, out, stats);
        cache.refresh(f, &snap);
        snap
    }

    #[test]
    fn cache_fetches_each_key_once() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(1, 1), 0, "a"),
            RawProc::new(k(2, 2), 1, "b"),
        ]);
        let mut c = StaticsCache::new();
        tick(&mut f, &mut c);
        tick(&mut f, &mut c);
        assert_eq!(f.statics_calls(), 2);
        assert_eq!(c.len(), 2);
    }

    #[test]
    fn cache_evicts_vanished_keys() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(1, 1), 0, "a"),
            RawProc::new(k(2, 2), 1, "b"),
        ]);
        f.push_frame(vec![RawProc::new(k(1, 1), 0, "a")]);
        let mut c = StaticsCache::new();
        tick(&mut f, &mut c);
        tick(&mut f, &mut c);
        assert!(c.get(k(2, 2)).is_none());
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn reused_pid_is_fetched_again() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(k(10, 1), 0, "old")]);
        f.push_frame(vec![RawProc::new(k(10, 7), 0, "new")]);
        let mut c = StaticsCache::new();
        tick(&mut f, &mut c);
        tick(&mut f, &mut c);
        assert_eq!(f.statics_calls(), 2);
        assert!(c.get(k(10, 1)).is_none());
        assert!(c.get(k(10, 7)).is_some());
    }

    #[test]
    fn restricted_statics_are_kept_and_never_fail_the_tick() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(k(4, 1), 0, "System")]);
        f.set_statics(k(4, 1), Statics::unavailable(&Field::Restricted));
        let mut c = StaticsCache::new();
        tick(&mut f, &mut c);
        let s = c.get(k(4, 1)).unwrap();
        assert_eq!(s.exe_path, Field::Restricted);
        assert_eq!(s.protection, Field::Restricted);
    }
}
