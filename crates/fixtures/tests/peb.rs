//! Tier-2 PEB read (cwd + environment) against a process this test spawned itself.
#![cfg(windows)]

use std::process::Command;

use atropos_core::Field;
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;

#[test]
fn peb_reads_cwd_and_env_of_sleeper() {
    let dir = std::env::temp_dir().join(format!("atropos peb ü {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut cmd = Command::new(atropos_fixtures::fixture_path("sleeper").unwrap());
    cmd.current_dir(&dir)
        .env("ATR_TEST", "42 ü")
        .env("GITHUB_TOKEN", "ghp_x");
    let child = atropos_fixtures::spawn_cmd(cmd, &["--secs", "60"]).unwrap();

    let got = WindowsPlatform::new().cwd_and_env(child.key);
    let env = got.ok().unwrap();
    let want = dir.display().to_string();
    assert!(
        env.cwd.trim_end_matches('\\').eq_ignore_ascii_case(&want),
        "{} vs {want}",
        env.cwd
    );
    assert!(env.vars.iter().any(|(n, v)| n == "ATR_TEST" && v == "42 ü"));
    assert!(env.vars.iter().any(|(n, _)| n == "GITHUB_TOKEN"));
    assert!(env.vars.iter().all(|(n, _)| !n.is_empty()));
    drop(child);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn peb_read_on_exited_process_is_gone() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let key = child.key;
    drop(child);
    assert_eq!(WindowsPlatform::new().cwd_and_env(key), Field::Gone);
}

#[test]
fn peb_read_of_a_session0_service_never_fails_the_caller() {
    let mut p = WindowsPlatform::new();
    let mut out = Vec::new();
    p.scan(&mut out).unwrap();
    // services.exe runs as SYSTEM; a standard user gets Restricted, an admin may get Ok.
    if let Some(svc) = out.iter().find(|r| &*r.image == "services.exe") {
        let r = p.cwd_and_env(svc.key);
        assert!(matches!(r, Field::Restricted | Field::Ok(_)), "{r:?}");
    }
}
