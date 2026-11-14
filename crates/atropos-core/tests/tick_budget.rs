//! Collector tick budget (spec §3.2: p95 ≤ 5 ms) on a fake 10 000-process machine.
//! Release-only: `cargo test --release -p atropos-core --test tick_budget -- --ignored`.

use std::time::Instant;

use atropos_core::ProcKey;
use atropos_core::platform::{FakePlatform, Platform, RawProc};
use atropos_core::snapshot::build;
use atropos_core::statics::StaticsCache;

#[test]
#[ignore = "timing; run in release"]
fn fake_10k_processes_tick_under_budget() {
    let mut f = FakePlatform::new(16);
    let procs: Vec<RawProc> = (1..=10_000u32)
        .map(|i| {
            let mut r = RawProc::new(
                ProcKey {
                    pid: i * 4,
                    create_time: u64::from(i),
                },
                (i / 3) * 4,
                "node.exe",
            );
            r.cpu_100ns = u64::from(i) * 1000;
            r
        })
        .collect();
    f.push_frame(procs);
    let mut cache = StaticsCache::new();
    let mut raw = Vec::new();
    let mut prev = None;
    let mut times = Vec::new();
    for _ in 0..40 {
        let t = Instant::now();
        let stats = f.scan(&mut raw).unwrap();
        let snap = build(prev.as_ref(), std::mem::take(&mut raw), stats);
        cache.refresh(&mut f, &snap);
        times.push(t.elapsed());
        prev = Some(snap);
    }
    times.sort();
    let p95 = times[times.len() * 95 / 100];
    println!("p95 = {p95:?}");
    assert!(p95.as_secs_f64() * 1000.0 <= 5.0, "p95 {p95:?}");
}
