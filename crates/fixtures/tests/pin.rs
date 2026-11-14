//! Pinning on the real backend, against processes this test spawned itself.
#![cfg(windows)]

use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::pin::{Access, PinError};
use atropos_core::platform::Platform;
use atropos_core::platform::windows::{WindowsPlatform, current_process_key};

#[test]
fn pin_sleeper_then_see_it_exit() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "1"]).unwrap();
    let mut p = WindowsPlatform::new();
    let pinned = p.pin(child.key, Access::QUERY).unwrap();
    assert_eq!(p.wait_exits(&[&pinned], Duration::ZERO), vec![false]);
    let t = Instant::now();
    assert_eq!(
        p.wait_exits(&[&pinned], Duration::from_secs(10)),
        vec![true]
    );
    assert!(t.elapsed() < Duration::from_secs(5));
}

#[test]
fn pin_with_wrong_create_time_is_reused() {
    let me = current_process_key().unwrap();
    let impostor = ProcKey {
        pid: me.pid,
        create_time: me.create_time + 1,
    };
    let err = WindowsPlatform::new()
        .pin(impostor, Access::QUERY)
        .unwrap_err();
    assert_eq!(err, PinError::Reused);
}

#[test]
fn pin_after_exit_is_gone_or_reused() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let key = child.key;
    drop(child);
    let err = WindowsPlatform::new().pin(key, Access::KILL).unwrap_err();
    assert!(matches!(err, PinError::Gone | PinError::Reused), "{err:?}");
}

#[test]
fn many_pins_wait_in_chunks() {
    // More than 64 handles: WaitForMultipleObjects must be chunked.
    let mut p = WindowsPlatform::new();
    let me = current_process_key().unwrap();
    let pins: Vec<_> = (0..70).map(|_| p.pin(me, Access::QUERY).unwrap()).collect();
    let refs: Vec<_> = pins.iter().collect();
    let got = p.wait_exits(&refs, Duration::from_millis(50));
    assert_eq!(got.len(), 70);
    assert!(got.iter().all(|e| !e));
}
