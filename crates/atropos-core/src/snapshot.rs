//! One immutable view of the machine: processes, validated tree, rollups and CPU %.

use std::collections::HashMap;

use crate::ProcKey;
use crate::cpu::cpu_pct;
use crate::platform::{RawProc, SystemStats};
use crate::tree;

/// Subtree totals, the process itself included.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rollup {
    pub cpu_pct: f32,
    pub private_ws: u64,
    pub count: u32,
}

/// A process in a [`Snapshot`].
#[derive(Debug, Clone, PartialEq)]
pub struct Proc {
    pub raw: RawProc,
    /// Index of the validated parent (see [`crate::tree::link`]).
    pub parent: Option<usize>,
    pub depth: u16,
    /// `None` on the first sample of this `ProcKey` (shown as `—`).
    pub cpu_pct: Option<f32>,
    /// Disk read / write bytes per second since the previous sample (`None` on the first).
    pub read_rate: Option<f32>,
    pub write_rate: Option<f32>,
    pub rollup: Rollup,
}

/// Processes plus their tree. `index` maps a key to its position in `procs`.
#[derive(Debug, Clone, Default)]
pub struct Snapshot {
    pub procs: Vec<Proc>,
    pub index: HashMap<ProcKey, usize>,
    pub roots: Vec<usize>,
    pub children: Vec<Vec<usize>>,
    pub stats: SystemStats,
}

impl Snapshot {
    /// The process with this exact identity, if it is in the snapshot.
    #[must_use]
    pub fn get(&self, key: ProcKey) -> Option<&Proc> {
        self.index.get(&key).map(|&i| &self.procs[i])
    }

    /// Whole-machine CPU % (sum over processes, Idle excluded); `None` on the first sample.
    #[must_use]
    pub fn total_cpu_pct(&self) -> Option<f32> {
        let mut any = false;
        let sum = self
            .procs
            .iter()
            .filter(|p| p.raw.key.pid != 0)
            .filter_map(|p| p.cpu_pct)
            .inspect(|_| any = true)
            .sum::<f32>();
        any.then_some(sum.min(100.0))
    }
}

/// Bytes per second between two cumulative counters; `None` without elapsed time.
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // display-only rate
fn rate(prev: u64, now: u64, dwall_100ns: u64) -> Option<f32> {
    (dwall_100ns > 0).then(|| (now.saturating_sub(prev) as f64 / (dwall_100ns as f64 / 1e7)) as f32)
}

/// Builds a snapshot from one scan. `prev` supplies the previous CPU sample per `ProcKey`.
/// Runs in O(n): one link pass, one DFS for depth, one reverse pass for rollups.
#[must_use]
pub fn build(prev: Option<&Snapshot>, raw: Vec<RawProc>, stats: SystemStats) -> Snapshot {
    let n = raw.len();
    let mut index = HashMap::with_capacity(n);
    let mut by_pid = HashMap::with_capacity(n);
    for (i, r) in raw.iter().enumerate() {
        index.insert(r.key, i);
        by_pid.insert(r.key.pid, i);
    }
    let parents = tree::link(&raw, &by_pid);
    let mut children = vec![Vec::new(); n];
    let mut roots = Vec::new();
    for (i, parent) in parents.iter().enumerate() {
        match parent {
            Some(j) => children[*j].push(i),
            None => roots.push(i),
        }
    }

    let dwall = prev.map_or(0, |p| stats.now_100ns.saturating_sub(p.stats.now_100ns));
    let mut procs: Vec<Proc> = raw
        .into_iter()
        .zip(parents)
        .map(|(raw, parent)| {
            let old = prev.and_then(|p| p.get(raw.key));
            let cpu = old.and_then(|old| {
                cpu_pct(old.raw.cpu_100ns, raw.cpu_100ns, dwall, stats.logical_cpus)
            });
            let read_rate = old.and_then(|o| rate(o.raw.read_bytes, raw.read_bytes, dwall));
            let write_rate = old.and_then(|o| rate(o.raw.write_bytes, raw.write_bytes, dwall));
            Proc {
                rollup: Rollup {
                    cpu_pct: cpu.unwrap_or(0.0),
                    private_ws: raw.private_ws,
                    count: 1,
                },
                read_rate,
                write_rate,
                raw,
                parent,
                depth: 0,
                cpu_pct: cpu,
            }
        })
        .collect();

    // Pre-order DFS from the roots: parents come before children, so depth is one pass and
    // rollups are one reverse pass. Every process is reachable: links strictly increase in age.
    let mut order = Vec::with_capacity(n);
    let mut stack: Vec<usize> = roots.iter().rev().copied().collect();
    while let Some(i) = stack.pop() {
        order.push(i);
        let depth = procs[i].depth.saturating_add(1);
        for &c in children[i].iter().rev() {
            procs[c].depth = depth;
            stack.push(c);
        }
    }
    for &i in order.iter().rev() {
        if let Some(parent) = procs[i].parent {
            let r = procs[i].rollup;
            let pr = &mut procs[parent].rollup;
            pr.cpu_pct += r.cpu_pct;
            pr.private_ws += r.private_ws;
            pr.count += r.count;
        }
    }

    Snapshot {
        procs,
        index,
        roots,
        children,
        stats,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn p(pid: u32, parent: u32, t: u64, cpu: u64, ws: u64) -> RawProc {
        let mut r = RawProc::new(
            ProcKey {
                pid,
                create_time: t,
            },
            parent,
            "x.exe",
        );
        r.cpu_100ns = cpu;
        r.private_ws = ws;
        r
    }

    fn stats(now: u64) -> SystemStats {
        SystemStats {
            logical_cpus: 4,
            total_ram: 100,
            used_ram: 50,
            now_100ns: now,
        }
    }

    #[test]
    fn cpu_is_none_on_first_sample() {
        let s = build(None, vec![p(10, 0, 1, 500, 0)], stats(10_000_000));
        assert_eq!(s.procs[0].cpu_pct, None);
        assert_eq!(s.total_cpu_pct(), None);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn cpu_uses_previous_sample_of_same_key() {
        let a = build(None, vec![p(10, 0, 1, 0, 0)], stats(10_000_000));
        let b = build(Some(&a), vec![p(10, 0, 1, 5_000_000, 0)], stats(20_000_000));
        assert_eq!(b.procs[0].cpu_pct, Some(12.5));
        assert_eq!(b.total_cpu_pct(), Some(12.5));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn disk_rates_are_bytes_per_second() {
        let mut first = p(10, 0, 1, 0, 0);
        first.read_bytes = 1000;
        first.write_bytes = 0;
        let a = build(None, vec![first.clone()], stats(10_000_000));
        assert_eq!(a.procs[0].read_rate, None);
        let mut next = first;
        next.read_bytes = 5000;
        next.write_bytes = 1000;
        // 2 s later.
        let b = build(Some(&a), vec![next], stats(30_000_000));
        assert_eq!(b.procs[0].read_rate, Some(2000.0));
        assert_eq!(b.procs[0].write_rate, Some(500.0));
    }

    #[test]
    fn reused_pid_gets_no_cpu_from_old_process() {
        let a = build(None, vec![p(10, 0, 1, 0, 0)], stats(10_000_000));
        let b = build(Some(&a), vec![p(10, 0, 7, 5_000_000, 0)], stats(20_000_000));
        assert_eq!(b.procs[0].cpu_pct, None);
    }

    #[test]
    fn tree_roots_children_and_depth() {
        let s = build(
            None,
            vec![
                p(1, 0, 1, 0, 0),
                p(2, 1, 2, 0, 0),
                p(3, 2, 3, 0, 0),
                p(9, 0, 4, 0, 0),
            ],
            stats(1),
        );
        assert_eq!(s.roots, vec![0, 3]);
        assert_eq!(s.children[0], vec![1]);
        assert_eq!(s.children[1], vec![2]);
        let depths: Vec<u16> = s.procs.iter().map(|p| p.depth).collect();
        assert_eq!(depths, vec![0, 1, 2, 0]);
        let k3 = ProcKey {
            pid: 3,
            create_time: 3,
        };
        assert_eq!(s.get(k3).map(|p| p.parent), Some(Some(1)));
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn rollup_sums_subtree() {
        let a = build(
            None,
            vec![p(1, 0, 1, 0, 100), p(2, 1, 2, 0, 10), p(3, 1, 3, 0, 1)],
            stats(10_000_000),
        );
        let b = build(
            Some(&a),
            vec![
                p(1, 0, 1, 4_000_000, 100),
                p(2, 1, 2, 4_000_000, 10),
                p(3, 1, 3, 0, 1),
            ],
            stats(20_000_000),
        );
        let r = b.procs[0].rollup;
        assert_eq!(r.count, 3);
        assert_eq!(r.private_ws, 111);
        assert_eq!(r.cpu_pct, 20.0);
        assert_eq!(b.procs[2].rollup.count, 1);
    }
}
