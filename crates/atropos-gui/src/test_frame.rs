//! A small, fixed machine for GUI tests.
//!
//! ```text
//! 1 explorer.exe ─┬ 2 node.exe (30 % CPU) ── 4 node.exe (5 %)
//!                 └ 3 Code.exe (10 %)
//! 9 svchost.exe (1 %, user Restricted)
//! ```

use std::sync::Arc;

use atropos_core::collector::Frame;
use atropos_core::platform::{RawProc, SystemStats};
use atropos_core::ring::Ring;
use atropos_core::snapshot::build;
use atropos_core::statics::{Statics, StaticsMap};
use atropos_core::{Field, ProcKey};

pub fn k(pid: u32) -> ProcKey {
    ProcKey {
        pid,
        create_time: u64::from(pid),
    }
}

pub fn frame() -> Frame {
    let mk = |pid, ppid, name: &str, cpu: u64, ws: u64| {
        let mut r = RawProc::new(k(pid), ppid, name);
        r.cpu_100ns = cpu;
        r.private_ws = ws;
        r.threads = 3;
        r
    };
    let stats = |now| SystemStats {
        logical_cpus: 1,
        total_ram: 1,
        used_ram: 1,
        now_100ns: now,
    };
    let procs = |cpu: [u64; 5]| {
        vec![
            mk(1, 0, "explorer.exe", cpu[0], 100),
            mk(2, 1, "node.exe", cpu[1], 300),
            mk(3, 1, "Code.exe", cpu[2], 200),
            mk(4, 2, "node.exe", cpu[3], 50),
            mk(9, 0, "svchost.exe", cpu[4], 10),
        ]
    };
    let s1 = build(None, procs([0; 5]), stats(10_000_000));
    let snapshot = build(
        Some(&s1),
        procs([0, 3_000_000, 1_000_000, 500_000, 100_000]),
        stats(20_000_000),
    );
    let mut statics = StaticsMap::new();
    for p in &snapshot.procs {
        let user = if p.raw.key.pid == 9 {
            Field::Restricted
        } else {
            Field::Ok(Arc::from("josé"))
        };
        statics.insert(
            p.raw.key,
            Arc::new(Statics {
                user,
                cmdline: Field::Ok(Arc::from(format!("{} --flag", p.raw.image))),
                ..Statics::unavailable(&Field::Restricted)
            }),
        );
    }
    Frame {
        snapshot,
        statics: Arc::new(statics),
        cpu_history: Ring::default(),
        mem_history: Ring::default(),
        tick_ms: 0.0,
        scan_ms: 0.0,
        env: None,
    }
}
