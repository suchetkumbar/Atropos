//! Suspend / resume a process or its whole subtree (spec Phase 2). Windows suspend counts nest;
//! the ledger tracks how many suspends Atropos applied so the UI can show "Suspended ×2".

use std::collections::HashMap;

use crate::ProcKey;
use crate::pin::{Access, PinError};
use crate::platform::Platform;
use crate::snapshot::Snapshot;

/// Result for one process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Toggle {
    Done,
    Denied,
    Protected,
    Gone,
}

/// Suspends Atropos applied and has not undone yet, per process.
#[derive(Debug, Default)]
pub struct SuspendLedger {
    counts: HashMap<ProcKey, u32>,
}

fn targets(snap: &Snapshot, root: ProcKey, tree: bool) -> Vec<ProcKey> {
    match snap.index.get(&root) {
        Some(&i) if tree => super::subtree::preorder(snap, i),
        _ => vec![root],
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dir {
    Suspend,
    Resume,
}

impl SuspendLedger {
    fn apply(
        &mut self,
        platform: &mut dyn Platform,
        keys: Vec<ProcKey>,
        dir: Dir,
        blocked: &dyn Fn(ProcKey) -> bool,
    ) -> Vec<(ProcKey, Toggle)> {
        keys.into_iter()
            .map(|key| {
                if blocked(key) {
                    return (key, Toggle::Protected);
                }
                let p = match platform.pin(key, Access::SUSPEND) {
                    Ok(p) => p,
                    Err(PinError::Gone | PinError::Reused) => return (key, Toggle::Gone),
                    Err(PinError::Denied | PinError::Os(_)) => return (key, Toggle::Denied),
                };
                let ok = match dir {
                    Dir::Suspend => platform.suspend(&p).is_ok(),
                    Dir::Resume => platform.resume(&p).is_ok(),
                };
                if !ok {
                    return (key, Toggle::Denied);
                }
                let n = self.counts.entry(key).or_default();
                match dir {
                    Dir::Suspend => *n += 1,
                    Dir::Resume => *n = n.saturating_sub(1),
                }
                if *n == 0 {
                    self.counts.remove(&key);
                }
                (key, Toggle::Done)
            })
            .collect()
    }

    /// How many outstanding suspends Atropos applied to `key`.
    #[must_use]
    pub fn count(&self, key: ProcKey) -> u32 {
        self.counts.get(&key).copied().unwrap_or(0)
    }

    /// Suspends `root` (and its subtree when `tree`), top-down.
    pub fn suspend(
        &mut self,
        platform: &mut dyn Platform,
        snap: &Snapshot,
        root: ProcKey,
        tree: bool,
        blocked: &dyn Fn(ProcKey) -> bool,
    ) -> Vec<(ProcKey, Toggle)> {
        self.apply(platform, targets(snap, root, tree), Dir::Suspend, blocked)
    }

    /// Resumes `root` (and its subtree when `tree`). Works on processes suspended by others too.
    pub fn resume(
        &mut self,
        platform: &mut dyn Platform,
        snap: &Snapshot,
        root: ProcKey,
        tree: bool,
        blocked: &dyn Fn(ProcKey) -> bool,
    ) -> Vec<(ProcKey, Toggle)> {
        self.apply(platform, targets(snap, root, tree), Dir::Resume, blocked)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc};
    use crate::snapshot::build;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    fn world() -> (FakePlatform, Snapshot) {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(10), 1, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(20), 1, "other.exe"),
        ]);
        let snap = scan(&mut f);
        (f, snap)
    }

    fn scan(f: &mut FakePlatform) -> Snapshot {
        let mut raw = Vec::new();
        let stats = f.scan(&mut raw).unwrap();
        build(None, raw, stats)
    }

    fn suspended(f: &mut FakePlatform, key: ProcKey) -> bool {
        scan(f).get(key).is_some_and(|p| p.raw.suspended)
    }

    #[test]
    fn suspend_then_resume_one_process() {
        let (mut f, snap) = world();
        let mut l = SuspendLedger::default();
        assert_eq!(
            l.suspend(&mut f, &snap, k(10), false, &|_| false),
            vec![(k(10), Toggle::Done)]
        );
        assert!(suspended(&mut f, k(10)));
        assert!(
            !suspended(&mut f, k(11)),
            "single-process suspend leaves children running"
        );
        assert_eq!(l.count(k(10)), 1);
        assert_eq!(
            l.resume(&mut f, &snap, k(10), false, &|_| false),
            vec![(k(10), Toggle::Done)]
        );
        assert!(!suspended(&mut f, k(10)));
        assert_eq!(l.count(k(10)), 0);
    }

    #[test]
    fn suspend_counts_nest() {
        let (mut f, snap) = world();
        let mut l = SuspendLedger::default();
        l.suspend(&mut f, &snap, k(10), false, &|_| false);
        l.suspend(&mut f, &snap, k(10), false, &|_| false);
        assert_eq!(l.count(k(10)), 2);
        l.resume(&mut f, &snap, k(10), false, &|_| false);
        assert!(
            suspended(&mut f, k(10)),
            "one resume of two suspends keeps it suspended"
        );
        assert_eq!(l.count(k(10)), 1);
        l.resume(&mut f, &snap, k(10), false, &|_| false);
        assert!(!suspended(&mut f, k(10)));
    }

    #[test]
    fn tree_suspend_covers_descendants_only() {
        let (mut f, snap) = world();
        let mut l = SuspendLedger::default();
        let r = l.suspend(&mut f, &snap, k(10), true, &|_| false);
        assert_eq!(r, vec![(k(10), Toggle::Done), (k(11), Toggle::Done)]);
        assert!(suspended(&mut f, k(11)));
        assert!(!suspended(&mut f, k(20)));
        l.resume(&mut f, &snap, k(10), true, &|_| false);
        assert!(!suspended(&mut f, k(10)) && !suspended(&mut f, k(11)));
    }

    #[test]
    fn blocked_denied_and_gone() {
        let (mut f, snap) = world();
        let mut l = SuspendLedger::default();
        assert_eq!(
            l.suspend(&mut f, &snap, k(10), false, &|key| key == k(10)),
            vec![(k(10), Toggle::Protected)]
        );
        f.deny(k(20));
        assert_eq!(
            l.suspend(&mut f, &snap, k(20), false, &|_| false),
            vec![(k(20), Toggle::Denied)]
        );
        f.exit(k(11));
        assert_eq!(
            l.suspend(&mut f, &snap, k(11), false, &|_| false),
            vec![(k(11), Toggle::Gone)]
        );
        assert_eq!(l.count(k(10)) + l.count(k(20)) + l.count(k(11)), 0);
    }
}
