//! The real helper (`Atropos.exe --elevated-helper`) over its real pipe. Started without UAC here
//! (the elevation itself is a manual QA item); it only ever acts on processes this test started.
#![allow(clippy::unwrap_used)] // test code

use std::os::windows::process::CommandExt as _;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::engine::kill::Outcome;
use atropos_core::platform::Platform;
use atropos_core::platform::windows::pipe::{current_user_sid, random_bytes};
use atropos_core::platform::windows::{WindowsPlatform, current_process_key};
use atropos_helper::client::connect;
use atropos_helper::host::hex;
use atropos_helper::protocol::{Op, Rejection, Reply};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

struct Helper {
    child: Child,
    pipe: String,
    nonce: [u8; 32],
}

impl Drop for Helper {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start_helper(parent: ProcKey, extra: &[&str]) -> Helper {
    let pipe = format!("atropos-test-{}", hex(&random_bytes::<8>().unwrap()));
    let nonce: [u8; 32] = random_bytes().unwrap();
    let child = Command::new(env!("CARGO_BIN_EXE_Atropos"))
        .args([
            "--elevated-helper",
            "--pipe",
            &pipe,
            "--nonce",
            &hex(&nonce),
            "--user-sid",
            &current_user_sid().unwrap(),
            "--parent",
            &format!("{}:{}", parent.pid, parent.create_time),
        ])
        .args(extra)
        .spawn()
        .unwrap();
    Helper { child, pipe, nonce }
}

/// A victim this test owns: `ping` on its own hidden console.
fn victim() -> (Child, ProcKey) {
    let c = Command::new("ping")
        .args(["-n", "60", "127.0.0.1"])
        .creation_flags(CREATE_NO_WINDOW)
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    std::thread::sleep(Duration::from_millis(200));
    let mut raw = Vec::new();
    WindowsPlatform::new().scan(&mut raw).unwrap();
    let key = raw.iter().find(|r| r.key.pid == c.id()).unwrap().key;
    (c, key)
}

#[test]
fn helper_terminates_an_allowed_process() {
    let h = start_helper(current_process_key().unwrap(), &[]);
    let (mut v, key) = victim();
    let mut s = connect(&h.pipe, h.nonce, Duration::from_secs(10)).unwrap();
    assert_eq!(
        s.send(Op::Terminate, key).unwrap(),
        Reply::Killed(Outcome::Terminated)
    );
    assert!(v.wait().unwrap().code().is_some());
}

#[test]
fn helper_rejects_a_bad_nonce() {
    let h = start_helper(current_process_key().unwrap(), &[]);
    let (mut v, key) = victim();
    let mut s = connect(&h.pipe, h.nonce, Duration::from_secs(10))
        .unwrap()
        .with_nonce([0; 32]);
    assert_eq!(
        s.send(Op::Terminate, key).unwrap(),
        Reply::Rejected(Rejection::BadNonce)
    );
    assert!(v.try_wait().unwrap().is_none(), "untouched");
    let _ = v.kill();
}

#[test]
fn helper_refuses_its_own_parent() {
    let me = current_process_key().unwrap();
    let h = start_helper(me, &[]);
    let mut s = connect(&h.pipe, h.nonce, Duration::from_secs(10)).unwrap();
    assert_eq!(
        s.send(Op::Terminate, me).unwrap(),
        Reply::Killed(Outcome::Protected)
    );
}

#[test]
fn helper_exits_when_idle() {
    let mut h = start_helper(current_process_key().unwrap(), &["--idle-secs", "1"]);
    let start = Instant::now();
    let status = loop {
        if let Some(s) = h.child.try_wait().unwrap() {
            break s;
        }
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "helper did not exit"
        );
        std::thread::sleep(Duration::from_millis(100));
    };
    assert!(status.success());
}

#[test]
fn helper_exits_when_its_parent_exits() {
    let (mut parent, parent_key) = victim();
    let mut h = start_helper(parent_key, &[]);
    std::thread::sleep(Duration::from_millis(500));
    assert!(
        h.child.try_wait().unwrap().is_none(),
        "serving while the parent lives"
    );
    parent.kill().unwrap();
    parent.wait().unwrap();
    let start = Instant::now();
    while h.child.try_wait().unwrap().is_none() {
        assert!(
            start.elapsed() < Duration::from_secs(10),
            "helper outlived its parent"
        );
        std::thread::sleep(Duration::from_millis(100));
    }
}

#[test]
fn bad_arguments_never_serve() {
    let o = Command::new(env!("CARGO_BIN_EXE_Atropos"))
        .args(["--elevated-helper", "--pipe", "x"])
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(2));
}
