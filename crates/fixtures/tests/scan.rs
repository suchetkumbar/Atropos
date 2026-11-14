//! The real Windows scan, checked against processes this test spawned itself.
#![cfg(windows)]

use atropos_core::platform::windows::{WindowsPlatform, current_process_key};
use atropos_core::platform::{Platform, RawProc};

fn scan_once() -> atropos_core::Result<(Vec<RawProc>, atropos_core::platform::SystemStats)> {
    let mut p = WindowsPlatform::new();
    let mut out = Vec::new();
    let stats = p.scan(&mut out)?;
    Ok((out, stats))
}

#[test]
fn scan_contains_spawned_sleeper_with_matching_create_time() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let (procs, _) = scan_once().unwrap();
    let me = procs.iter().find(|p| p.key.pid == child.key.pid).unwrap();
    assert_eq!(
        me.key, child.key,
        "create_time must match the fixture's own report"
    );
    assert_eq!(&*me.image, "sleeper.exe");
    assert_eq!(me.ppid, std::process::id());
    assert!(me.threads >= 1);
    assert!(me.private_ws > 0);
}

#[test]
fn scan_contains_self_with_live_counters() {
    let key = current_process_key().unwrap();
    // Burn a little CPU and do some I/O so the counters are non-zero.
    let mut x = 0u64;
    for i in 0..5_000_000u64 {
        x = x.wrapping_mul(31).wrapping_add(i);
    }
    std::hint::black_box(x);
    let _ = std::fs::read(std::env::current_exe().unwrap()).unwrap();
    let (procs, stats) = scan_once().unwrap();
    let me = procs.iter().find(|p| p.key == key).unwrap();
    assert!(me.cpu_100ns > 0);
    assert!(me.handles > 0);
    assert!(me.read_bytes > 0);
    assert!(!me.suspended);
    assert!(stats.logical_cpus >= 1);
    assert!(stats.total_ram > stats.used_ram && stats.used_ram > 0);
    assert!(stats.now_100ns > key.create_time);
}

#[test]
fn scan_names_idle_and_system() {
    let (procs, _) = scan_once().unwrap();
    assert!(
        procs
            .iter()
            .any(|p| p.key.pid == 0 && &*p.image == "System Idle Process")
    );
    assert!(
        procs
            .iter()
            .any(|p| p.key.pid == 4 && &*p.image == "System")
    );
    assert!(procs.len() > 20);
}

#[test]
fn image_names_are_unicode_safe() {
    let dir = std::env::temp_dir().join(format!("atropos scan ü {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = dir.join("slé ep er.exe");
    std::fs::copy(atropos_fixtures::fixture_path("sleeper").unwrap(), &exe).unwrap();
    let child = atropos_fixtures::spawn_exe(&exe, &["--secs", "60"]).unwrap();
    let (procs, _) = scan_once().unwrap();
    let me = procs.iter().find(|p| p.key == child.key).unwrap();
    assert_eq!(&*me.image, "slé ep er.exe");
    drop(child);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn rescans_reuse_the_platform_buffer() {
    let mut p = WindowsPlatform::new();
    let mut out = Vec::new();
    for _ in 0..5 {
        p.scan(&mut out).unwrap();
        assert!(out.len() > 20);
    }
}
