//! Graceful strategies and escalation on the real backend, only against fixtures this test
//! spawned. Console fixtures get their own hidden console (`CREATE_NO_WINDOW`), so a Ctrl+C sent
//! to them can never reach the test runner's console.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // test code; helpers shared by several #[test]s

use std::os::windows::process::CommandExt as _;
use std::process::Command;
use std::time::Duration;

use atropos_core::engine::kill::{KillOpts, Outcome, Strategy, kill};
use atropos_core::pin::Access;
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_fixtures::{SpawnedFixture, fixture_path, spawn_cmd};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn own_console(name: &str) -> SpawnedFixture {
    let mut cmd = Command::new(fixture_path(name).unwrap());
    cmd.creation_flags(CREATE_NO_WINDOW);
    spawn_cmd(cmd, &["--secs", "60"]).unwrap()
}

fn platform() -> WindowsPlatform {
    WindowsPlatform::new().with_ctrl_c_helper(fixture_path("ctrlc-helper").unwrap())
}

fn quick() -> KillOpts {
    KillOpts {
        grace: Duration::from_secs(3),
        force: false,
    }
}

#[test]
fn console_app_exits_on_ctrl_c() {
    let child = own_console("sleeper");
    let mut p = platform();
    let pinned = p.pin(child.key, Access::QUERY).unwrap();
    assert_eq!(p.strategy(&pinned), Strategy::ConsoleCtrlC);
    drop(pinned);
    let outcome = kill(&mut p, child.key, quick(), false, &mut |_| {});
    assert_eq!(outcome, Outcome::Exited);
}

#[test]
fn console_app_ignoring_ctrl_c_is_forced() {
    let child = own_console("ignore-ctrlc");
    let outcome = kill(&mut platform(), child.key, quick(), false, &mut |_| {});
    assert_eq!(outcome, Outcome::Terminated);
}

#[test]
fn gui_window_closes_on_wm_close() {
    let child = atropos_fixtures::spawn("gui-window", &["--secs", "60"]).unwrap();
    let mut p = platform();
    // The window may take a moment to appear after the key line.
    let mut strategy = Strategy::None;
    for _ in 0..50 {
        let pinned = p.pin(child.key, Access::QUERY).unwrap();
        strategy = p.strategy(&pinned);
        if strategy == Strategy::Window {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    assert_eq!(strategy, Strategy::Window);
    let outcome = kill(&mut p, child.key, quick(), false, &mut |_| {});
    assert_eq!(outcome, Outcome::Exited);
}

#[test]
fn without_the_helper_console_apps_are_forced() {
    let child = own_console("sleeper");
    let mut p = WindowsPlatform::new();
    let pinned = p.pin(child.key, Access::QUERY).unwrap();
    assert_eq!(p.strategy(&pinned), Strategy::None);
    drop(pinned);
    let outcome = kill(&mut p, child.key, quick(), false, &mut |_| {});
    assert_eq!(outcome, Outcome::Terminated);
}

#[test]
fn force_terminates_without_asking() {
    let child = own_console("sleeper");
    let opts = KillOpts {
        force: true,
        ..quick()
    };
    assert_eq!(
        kill(&mut platform(), child.key, opts, false, &mut |_| {}),
        Outcome::Terminated
    );
}

#[test]
fn killing_an_exited_fixture_is_gone() {
    let child = own_console("sleeper");
    let key = child.key;
    drop(child);
    assert_eq!(
        kill(&mut platform(), key, quick(), false, &mut |_| {}),
        Outcome::Gone
    );
}
