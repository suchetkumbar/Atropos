//! Property tests for the snapshot tree (spec §4.2).

use atropos_core::ProcKey;
use atropos_core::platform::{RawProc, SystemStats};
use atropos_core::snapshot::build;
use proptest::prelude::*;

/// Up to 60 processes with unique PIDs and random parents (often missing, often younger) and times.
fn procs() -> impl Strategy<Value = Vec<RawProc>> {
    prop::collection::vec((0u32..80, 0u64..50, 0u64..1000), 0..60).prop_map(|v| {
        v.into_iter()
            .zip(1u32..)
            .map(|((ppid, t, ws), i)| {
                let mut r = RawProc::new(
                    ProcKey {
                        pid: i * 4,
                        create_time: t,
                    },
                    ppid * 4,
                    "p.exe",
                );
                r.private_ws = ws;
                r
            })
            .collect()
    })
}

proptest! {
    #[test]
    fn no_cycles_and_every_child_starts_after_parent(raw in procs()) {
        let s = build(None, raw, SystemStats::default());
        for (i, p) in s.procs.iter().enumerate() {
            let mut cur = i;
            let mut steps = 0usize;
            while let Some(par) = s.procs[cur].parent {
                prop_assert!(s.procs[par].raw.key.create_time < s.procs[cur].raw.key.create_time);
                cur = par;
                steps += 1;
                prop_assert!(steps <= s.procs.len());
            }
            prop_assert_eq!(usize::from(p.depth), steps);
        }
    }

    #[test]
    fn rollup_count_and_memory_equal_subtree(raw in procs()) {
        let s = build(None, raw, SystemStats::default());
        for i in 0..s.procs.len() {
            let (mut count, mut ws, mut stack) = (0u32, 0u64, vec![i]);
            while let Some(j) = stack.pop() {
                count += 1;
                ws += s.procs[j].raw.private_ws;
                stack.extend(&s.children[j]);
            }
            prop_assert_eq!(s.procs[i].rollup.count, count);
            prop_assert_eq!(s.procs[i].rollup.private_ws, ws);
        }
        let roots = s.procs.iter().filter(|p| p.parent.is_none()).count();
        prop_assert_eq!(roots, s.roots.len());
    }
}
