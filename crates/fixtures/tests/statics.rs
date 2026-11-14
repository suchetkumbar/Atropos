//! Tier-1 statics on the real Windows backend, against processes this test spawned itself.
#![cfg(windows)]

use atropos_core::platform::Platform;
use atropos_core::platform::windows::{WindowsPlatform, current_process_key};
use atropos_core::statics::Protection;
use atropos_core::{Field, ProcKey};

fn username() -> String {
    std::env::var("USERNAME").unwrap_or_default().to_lowercase()
}

#[test]
fn sleeper_statics_have_exe_path_cmdline_and_user() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60", "ä b"]).unwrap();
    let s = WindowsPlatform::new().statics(child.key);
    let exe = atropos_fixtures::fixture_path("sleeper").unwrap();
    let got = s.exe_path.ok().unwrap();
    assert!(
        got.eq_ignore_ascii_case(&exe.display().to_string()),
        "{got} vs {}",
        exe.display()
    );
    let cmd = s.cmdline.ok().unwrap();
    assert!(cmd.contains("--secs 60"), "{cmd}");
    assert!(cmd.contains("ä b"), "{cmd}");
    assert_eq!(s.user.ok().map(|u| u.to_lowercase()), Some(username()));
    assert_eq!(s.wow64, Field::Ok(false));
    assert_eq!(s.protection, Field::Ok(Protection::None));
}

#[test]
fn exited_process_is_gone() {
    let child = atropos_fixtures::spawn("sleeper", &["--secs", "60"]).unwrap();
    let key = child.key;
    drop(child); // kills and reaps
    let s = WindowsPlatform::new().statics(key);
    assert_eq!(s.exe_path, Field::Gone);
    assert_eq!(s.user, Field::Gone);
}

#[test]
fn wrong_create_time_is_gone() {
    let me = current_process_key().unwrap();
    let impostor = ProcKey {
        pid: me.pid,
        create_time: me.create_time + 1,
    };
    let s = WindowsPlatform::new().statics(impostor);
    assert_eq!(s.exe_path, Field::Gone);
    assert_eq!(s.cmdline, Field::Gone);
}

#[test]
fn every_process_reads_without_failing_and_own_session_is_readable() {
    let mut p = WindowsPlatform::new();
    let mut out = Vec::new();
    p.scan(&mut out).unwrap();
    let me = current_process_key().unwrap();
    let my_session = out.iter().find(|r| r.key == me).unwrap().session;
    let (mut mine, mut readable) = (0, 0);
    for raw in &out {
        let s = p.statics(raw.key);
        if raw.key.pid == 0 {
            assert!(!s.exe_path.is_ok());
        }
        if raw.session == my_session {
            mine += 1;
            readable += usize::from(s.exe_path.is_ok());
        }
    }
    // Session-0 services are `Restricted` for a standard user; our own session is readable.
    assert!(readable * 10 >= mine * 9, "{readable} of {mine}");
}
