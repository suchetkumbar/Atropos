//! Kill history + Restart on the real backend (spec Phase 6), against a `sleeper` this test
//! spawned. The restarted process is ended by the test through its own key.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::time::Duration;

use atropos_core::engine::kill::{KillOpts, Outcome, kill};
use atropos_core::history::{self, Launch};
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::{Field, ProcKey};

/// Waits until the statics of `key` are readable (a fresh process may not have its PEB yet).
fn cmdline_of(p: &mut WindowsPlatform, key: ProcKey) -> String {
    for _ in 0..50 {
        if let Field::Ok(c) = p.statics(key).cmdline {
            return c.to_string();
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("no command line for {key:?}");
}

fn end(p: &mut WindowsPlatform, key: ProcKey) {
    let opts = KillOpts {
        grace: Duration::from_millis(200),
        force: true,
    };
    let o = kill(p, key, opts, false, &mut |_| {});
    assert!(matches!(o, Outcome::Terminated | Outcome::Exited), "{o:?}");
}

#[test]
fn a_killed_sleeper_restarts_with_the_same_command_line_cwd_and_environment() {
    let dir = std::env::temp_dir().join(format!("atropos restart tëst {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = atropos_fixtures::fixture_path("sleeper").unwrap();
    let mut cmd = std::process::Command::new(&exe);
    cmd.current_dir(&dir).env("ATROPOS_RESTART_PROBE", "ü 1");
    let child = atropos_fixtures::spawn_cmd(cmd, &["--secs", "60", "a b", "ü \"q\""]).unwrap();
    let mut p = WindowsPlatform::new();

    // Captured before the kill, as the GUI does.
    let statics = p.statics(child.key);
    let launch: Launch = history::capture(&statics, &p.cwd_and_env(child.key)).unwrap();
    assert!(launch.cmdline.contains("\"a b\""), "{}", launch.cmdline);
    end(&mut p, child.key);
    drop(child);

    let new = history::restart(&mut p, &launch, false).unwrap();
    assert_ne!(new.pid, 0);
    assert_eq!(cmdline_of(&mut p, new), launch.cmdline);
    let env = p.cwd_and_env(new).ok().cloned().unwrap();
    assert_eq!(&*env.cwd, launch.cwd.as_str());
    assert!(
        env.vars
            .iter()
            .any(|(k, v)| k == "ATROPOS_RESTART_PROBE" && v == "ü 1"),
        "environment not carried over"
    );
    end(&mut p, new);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn restart_refuses_when_the_folder_was_deleted_and_starts_nothing() {
    let dir = std::env::temp_dir().join(format!("atropos restart gone {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let exe = atropos_fixtures::fixture_path("sleeper").unwrap();
    let mut cmd = std::process::Command::new(&exe);
    cmd.current_dir(&dir);
    let child = atropos_fixtures::spawn_cmd(cmd, &["--secs", "60"]).unwrap();
    let mut p = WindowsPlatform::new();
    let launch = history::capture(&p.statics(child.key), &p.cwd_and_env(child.key)).unwrap();
    end(&mut p, child.key);
    drop(child);
    std::thread::sleep(Duration::from_millis(100));
    std::fs::remove_dir_all(&dir).unwrap();
    let e = history::restart(&mut p, &launch, false).unwrap_err();
    assert!(e.contains("folder"), "{e}");
}
