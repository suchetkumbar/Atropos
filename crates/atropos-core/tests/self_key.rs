#![cfg(windows)]

use atropos_core::platform::windows::{current_process_key, now_filetime};

#[test]
fn self_key_matches_std_pid_and_is_in_the_past() {
    let k = current_process_key().unwrap();
    assert_eq!(k.pid, std::process::id());
    let now = now_filetime();
    assert!(k.create_time > 0 && k.create_time <= now);
}

#[test]
fn self_key_is_stable_across_calls() {
    let a = current_process_key().unwrap();
    let b = current_process_key().unwrap();
    assert_eq!(a, b);
}
