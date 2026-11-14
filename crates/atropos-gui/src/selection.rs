//! Selection by identity and the row-order freeze (spec Phase 1: "sorting never moves the
//! selection"; rows hold still under the pointer and reorder at most every 2 s).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::snapshot::Snapshot;

/// Minimum time between two reorders while values change.
pub const REORDER_EVERY: Duration = Duration::from_secs(2);

/// The selected process, by `ProcKey`: re-sorting cannot move it to another process.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Selection(pub Option<ProcKey>);

impl Selection {
    /// Clears the selection when its process left the snapshot.
    pub fn retain(&mut self, snap: &Snapshot) {
        if self.0.is_some_and(|k| !snap.index.contains_key(&k)) {
            self.0 = None;
        }
    }
}

/// Keeps the visible order stable: frozen while `hold` (pointer over the table, a menu or a
/// dialog is open) or `full` (`Space`), otherwise re-sorted at most every [`REORDER_EVERY`].
#[derive(Debug, Default)]
pub struct OrderFreeze {
    order: Vec<ProcKey>,
    last: Option<Instant>,
    /// One-shot: the next `apply` takes the fresh order even while held or frozen.
    force: bool,
    /// Full freeze toggled by `Space`.
    pub full: bool,
}

impl OrderFreeze {
    /// Forces the next [`Self::apply`] to take the fresh order, even under the pointer or a full
    /// freeze: the user changed the sort, filter or tree, or the first CPU sample arrived.
    pub fn invalidate(&mut self) {
        self.force = true;
    }

    /// Returns `fresh` re-ordered to match the frozen order when frozen: known items keep their
    /// place, items that vanished are dropped, new items are appended in fresh order.
    pub fn apply<T>(
        &mut self,
        fresh: Vec<T>,
        key: impl Fn(&T) -> ProcKey,
        hold: bool,
        now: Instant,
    ) -> Vec<T> {
        let due = self
            .last
            .is_none_or(|t| now.saturating_duration_since(t) >= REORDER_EVERY);
        if self.force || (due && !(self.full || hold)) {
            self.force = false;
            self.order = fresh.iter().map(&key).collect();
            self.last = Some(now);
            return fresh;
        }
        let pos: HashMap<ProcKey, usize> = self
            .order
            .iter()
            .enumerate()
            .map(|(i, k)| (*k, i))
            .collect();
        let mut known = Vec::with_capacity(fresh.len());
        let mut new = Vec::new();
        for item in fresh {
            match pos.get(&key(&item)) {
                Some(&i) => known.push((i, item)),
                None => new.push(item),
            }
        }
        known.sort_by_key(|(i, _)| *i);
        let out: Vec<T> = known.into_iter().map(|(_, t)| t).chain(new).collect();
        self.order = out.iter().map(&key).collect();
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    fn keys(v: &[ProcKey]) -> Vec<u32> {
        v.iter().map(|k| k.pid).collect()
    }

    #[test]
    fn reorder_at_most_every_2s() {
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        assert_eq!(keys(&f.apply(vec![k(1), k(2)], |k| *k, false, t0)), [1, 2]);
        let t1 = t0 + Duration::from_millis(500);
        assert_eq!(keys(&f.apply(vec![k(2), k(1)], |k| *k, false, t1)), [1, 2]);
        let t2 = t0 + REORDER_EVERY;
        assert_eq!(keys(&f.apply(vec![k(2), k(1)], |k| *k, false, t2)), [2, 1]);
    }

    #[test]
    fn hover_freezes_row_order() {
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        f.apply(vec![k(1), k(2), k(3)], |k| *k, false, t0);
        let later = t0 + Duration::from_secs(10);
        let got = f.apply(vec![k(3), k(2), k(1)], |k| *k, true, later);
        assert_eq!(keys(&got), [1, 2, 3]);
    }

    #[test]
    fn space_freezes_all() {
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        f.apply(vec![k(1), k(2)], |k| *k, false, t0);
        f.full = true;
        let got = f.apply(
            vec![k(2), k(1)],
            |k| *k,
            false,
            t0 + Duration::from_secs(60),
        );
        assert_eq!(keys(&got), [1, 2]);
    }

    #[test]
    fn frozen_order_drops_gone_and_appends_new() {
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        f.apply(vec![k(1), k(2), k(3)], |k| *k, false, t0);
        let got = f.apply(vec![k(4), k(3), k(1)], |k| *k, true, t0);
        assert_eq!(keys(&got), [1, 3, 4]);
    }

    #[test]
    fn invalidate_reorders_immediately() {
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        f.apply(vec![k(1), k(2)], |k| *k, false, t0);
        f.invalidate();
        let got = f.apply(vec![k(2), k(1)], |k| *k, false, t0);
        assert_eq!(keys(&got), [2, 1]);
    }

    #[test]
    fn invalidate_overrides_hover_and_full_freeze() {
        // Clicking a column header happens with the pointer over the table.
        let mut f = OrderFreeze::default();
        let t0 = Instant::now();
        f.apply(vec![k(1), k(2)], |k| *k, false, t0);
        f.full = true;
        f.invalidate();
        let got = f.apply(vec![k(2), k(1)], |k| *k, true, t0);
        assert_eq!(keys(&got), [2, 1]);
        // The forced reorder is one-shot: the hold applies again afterwards.
        let got = f.apply(vec![k(1), k(2)], |k| *k, true, t0 + REORDER_EVERY);
        assert_eq!(keys(&got), [2, 1]);
    }

    #[test]
    fn selection_is_cleared_when_its_process_exits() {
        use atropos_core::platform::{RawProc, SystemStats};
        let snap = atropos_core::snapshot::build(
            None,
            vec![RawProc::new(k(1), 0, "a.exe")],
            SystemStats::default(),
        );
        let mut s = Selection(Some(k(1)));
        s.retain(&snap);
        assert_eq!(s, Selection(Some(k(1))));
        let mut gone = Selection(Some(k(9)));
        gone.retain(&snap);
        assert_eq!(gone, Selection(None));
    }
}
