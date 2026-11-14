//! Subtree kill and console listing on the real backend, only against fixtures this test spawned.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // test code

use std::os::windows::process::CommandExt as _;
use std::process::Command;
use std::time::Duration;

use atropos_core::engine::kill::{KillOpts, Outcome};
use atropos_core::engine::subtree::kill_tree;
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::snapshot::build;
use atropos_fixtures::{SpawnedFixture, fixture_path, spawn_cmd};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

fn forker(max: &str) -> SpawnedFixture {
    let mut cmd = Command::new(fixture_path("forker").unwrap());
    cmd.creation_flags(CREATE_NO_WINDOW);
    spawn_cmd(cmd, &["--max", max, "--secs", "60"]).unwrap()
}

fn platform() -> WindowsPlatform {
    WindowsPlatform::new().with_ctrl_c_helper(fixture_path("ctrlc-helper").unwrap())
}

/// Live descendants of `root` (validated links) in a fresh snapshot.
fn descendants(p: &mut WindowsPlatform, root: atropos_core::ProcKey) -> usize {
    let mut raw = Vec::new();
    let stats = p.scan(&mut raw).unwrap();
    let s = build(None, raw, stats);
    s.procs
        .iter()
        .filter(|q| {
            let mut up = q.parent;
            while let Some(i) = up {
                if s.procs[i].raw.key == root {
                    return true;
                }
                up = s.procs[i].parent;
            }
            false
        })
        .count()
}

#[test]
fn forker_leaves_no_survivors() {
    let root = forker("40");
    std::thread::sleep(Duration::from_millis(800));
    let mut p = platform();
    assert!(
        descendants(&mut p, root.key) > 0,
        "the forker should have children by now"
    );
    let opts = KillOpts {
        grace: Duration::from_secs(2),
        force: false,
    };
    let report = kill_tree(&mut p, root.key, opts, &|_| false, &mut |_| {});
    assert_eq!(report.count(Outcome::Stuck), 0, "{}", report.summary());
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(descendants(&mut p, root.key), 0, "{}", report.summary());
    assert!(p.pin(root.key, atropos_core::pin::Access::QUERY).is_err());
}

#[test]
fn forced_tree_kill_terminates_frozen_members() {
    let root = forker("10");
    std::thread::sleep(Duration::from_millis(600));
    let mut p = platform();
    let opts = KillOpts {
        grace: Duration::from_secs(2),
        force: true,
    };
    let report = kill_tree(&mut p, root.key, opts, &|_| false, &mut |_| {});
    assert!(
        report.count(Outcome::Terminated) >= 2,
        "{}",
        report.summary()
    );
    assert_eq!(report.count(Outcome::Stuck), 0);
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(descendants(&mut p, root.key), 0);
}

#[test]
fn console_list_contains_the_forker_and_its_children() {
    let root = forker("5");
    std::thread::sleep(Duration::from_millis(800));
    let pids = platform().console_processes(root.key).unwrap();
    assert!(pids.contains(&root.key.pid), "{pids:?}");
    assert!(pids.len() >= 3, "forker + children: {pids:?}");
}
