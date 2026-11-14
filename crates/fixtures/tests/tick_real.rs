//! Collector tick budget on the real Windows backend, with a breakdown. Per ADR 0007 the
//! ≤ 5 ms p95 budget covers Atropos's own work (build + statics); the OS scan call is reported.
//! Release + on demand: `cargo test --release -p atropos-fixtures --test tick_real -- --ignored --nocapture`
#![cfg(windows)]

use std::time::{Duration, Instant};

use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::snapshot::build;
use atropos_core::statics::StaticsCache;

fn p95(v: &mut [Duration]) -> Duration {
    v.sort();
    v[v.len() * 95 / 100]
}

#[test]
#[ignore = "timing; run in release"]
fn real_tick_own_work_p95_within_budget() {
    let mut p = WindowsPlatform::new();
    let mut cache = StaticsCache::new();
    let mut prev = None;
    let (mut scan_t, mut build_t, mut statics_t, mut own) = (vec![], vec![], vec![], vec![]);
    for i in 0..60 {
        let t0 = Instant::now();
        let mut raw = Vec::with_capacity(512);
        let stats = p.scan(&mut raw).unwrap();
        let t1 = Instant::now();
        let snap = build(prev.as_ref(), raw, stats);
        let t2 = Instant::now();
        cache.refresh(&mut p, &snap);
        let t3 = Instant::now();
        if i > 0 {
            // The first tick reads statics for every process once; steady state is what counts.
            scan_t.push(t1 - t0);
            build_t.push(t2 - t1);
            statics_t.push(t3 - t2);
            own.push(t3 - t1);
        }
        prev = Some(snap);
        std::thread::sleep(Duration::from_millis(50));
    }
    println!(
        "p95: scan (OS call) {:?}, build {:?}, statics {:?}, own work {:?}",
        p95(&mut scan_t),
        p95(&mut build_t),
        p95(&mut statics_t),
        p95(&mut own)
    );
    assert!(p95(&mut own) <= Duration::from_millis(5));
}
