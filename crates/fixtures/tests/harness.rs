//! The harness only ever acts on processes it spawned itself (safety rule).

use std::process::Command;

/// Read-only liveness probe via `ps` (Unix); never signals anything.
#[cfg(not(windows))]
fn process_alive(pid: u32) -> bool {
    Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "pid="])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).trim() == pid.to_string())
}

/// Read-only liveness probe via `tasklist`; never signals anything.
#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH", "/FO", "CSV"])
        .output()
        .is_ok_and(|out| String::from_utf8_lossy(&out.stdout).contains(&format!("\"{pid}\"")))
}

#[test]
fn spawned_sleeper_reports_its_own_key() {
    let f = atropos_fixtures::spawn("sleeper", &["--secs", "30"]).unwrap();
    assert!(f.key.pid != 0 && f.key.create_time > 0);
    assert_eq!(f.key.pid, f.pid());
}

#[test]
fn drop_ends_only_the_spawned_child() {
    let f = atropos_fixtures::spawn("sleeper", &[]).unwrap();
    let pid = f.key.pid;
    assert!(process_alive(pid));
    drop(f);
    assert!(!process_alive(pid));
}

#[test]
fn unknown_fixture_has_actionable_error() {
    let e = atropos_fixtures::spawn("nope", &[])
        .unwrap_err()
        .to_string();
    assert!(e.contains("cargo build -p atropos-fixtures"), "{e}");
}
