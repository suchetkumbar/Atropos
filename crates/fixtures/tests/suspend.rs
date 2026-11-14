//! Suspend / resume on the real backend, only against a fixture this test spawned.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // test code

use atropos_core::ProcKey;
use atropos_core::engine::suspend::{SuspendLedger, Toggle};
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::snapshot::{Snapshot, build};

fn snap(p: &mut WindowsPlatform) -> Snapshot {
    let mut raw = Vec::new();
    let stats = p.scan(&mut raw).unwrap();
    build(None, raw, stats)
}

fn suspended(p: &mut WindowsPlatform, key: ProcKey) -> bool {
    snap(p).get(key).unwrap().raw.suspended
}

#[test]
fn suspend_shows_in_the_scan_and_counts_nest() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let mut p = WindowsPlatform::new();
    let mut l = SuspendLedger::default();
    assert!(!suspended(&mut p, child.key));
    let s = snap(&mut p);
    assert_eq!(
        l.suspend(&mut p, &s, child.key, false, &|_| false),
        vec![(child.key, Toggle::Done)]
    );
    l.suspend(&mut p, &s, child.key, false, &|_| false);
    assert!(suspended(&mut p, child.key));
    l.resume(&mut p, &s, child.key, false, &|_| false);
    assert!(
        suspended(&mut p, child.key),
        "nested: still suspended after one resume"
    );
    l.resume(&mut p, &s, child.key, false, &|_| false);
    assert!(!suspended(&mut p, child.key));
    assert_eq!(l.count(child.key), 0);
}
