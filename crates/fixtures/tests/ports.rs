//! Port → owner lookup on the real backend, against listeners this test spawned itself.
#![cfg(windows)]

use std::time::Duration;

use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::ports::{Proto, TcpState, owners};
use atropos_core::snapshot::build;

#[allow(clippy::unwrap_used)] // test helper shared by the four #[test]s below
fn check(proto: &str, ip: &str) {
    let child = atropos_fixtures::spawn(
        "port-listener",
        &["--proto", proto, "--ip", ip, "--secs", "60"],
    )
    .unwrap();
    let port: u16 = child
        .next_line(Duration::from_secs(5))
        .and_then(|l| l.trim().strip_prefix("port=").map(str::to_owned))
        .and_then(|p| p.parse().ok())
        .unwrap();
    let mut p = WindowsPlatform::new();
    let mut raw = Vec::new();
    let stats = p.scan(&mut raw).unwrap();
    let snap = build(None, raw, stats);
    let sockets = p.sockets().unwrap();
    let found = owners(&snap, &sockets, port);
    let (key, sock) = found
        .iter()
        .find(|(k, _)| *k == child.key)
        .unwrap_or_else(|| panic!("{proto}/v{ip} port {port} not found: {found:?}"));
    assert_eq!(*key, child.key);
    assert_eq!(sock.local.is_ipv6(), ip == "6");
    if proto == "udp" {
        assert_eq!(sock.proto, Proto::Udp);
        assert_eq!(sock.state, None);
    } else {
        assert_eq!(sock.proto, Proto::Tcp);
        assert_eq!(sock.state, Some(TcpState::Listen));
    }
}

#[test]
fn port_listener_found_on_tcp_v4() {
    check("tcp", "4");
}

#[test]
fn port_listener_found_on_tcp_v6() {
    check("tcp", "6");
}

#[test]
fn port_listener_found_on_udp_v4() {
    check("udp", "4");
}

#[test]
fn port_listener_found_on_udp_v6() {
    check("udp", "6");
}

#[test]
fn services_list_includes_a_known_service_host() {
    let services = WindowsPlatform::new().services().unwrap();
    assert!(services.len() > 10);
    // Every running service has a host PID; names are non-empty.
    assert!(services.iter().all(|s| s.pid > 0 && !s.name.is_empty()));
}
