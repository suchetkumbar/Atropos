//! Reverse lookup from a port to the process that owns it (spec Phase 2).

use std::net::SocketAddr;
use std::sync::Arc;

use crate::ProcKey;
use crate::snapshot::Snapshot;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Proto {
    Tcp,
    Udp,
}

/// TCP connection state (`MIB_TCP_STATE`); UDP sockets have none.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TcpState {
    Listen,
    Established,
    /// Any other `MIB_TCP_STATE` value (closing, time-wait, …).
    Other(u32),
}

/// One socket and the PID that owns it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Socket {
    pub proto: Proto,
    pub local: SocketAddr,
    /// TCP only, and only when connected.
    pub remote: Option<SocketAddr>,
    pub state: Option<TcpState>,
    pub owner: u32,
}

/// A Windows service and the process hosting it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceInfo {
    pub pid: u32,
    pub name: Arc<str>,
    pub display: Arc<str>,
}

/// A process that only forwards a port for something else.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Badge {
    /// `docker-proxy` / `com.docker.backend.exe`: the real listener is inside a container.
    DockerForward,
    /// `wslrelay.exe`: the real listener is inside WSL.
    WslRelay,
}

impl Badge {
    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Badge::DockerForward => "Docker port forward",
            Badge::WslRelay => "WSL relay",
        }
    }
}

/// Forwarded-port badge for an image name (case-insensitive), if any.
#[must_use]
pub fn badge(image: &str) -> Option<Badge> {
    let lower = image.to_ascii_lowercase();
    let stem = lower.strip_suffix(".exe").unwrap_or(&lower);
    match stem {
        "docker-proxy" | "com.docker.backend" => Some(Badge::DockerForward),
        "wslrelay" => Some(Badge::WslRelay),
        _ => None,
    }
}

/// A port as stored in the `MIB_*ROW_OWNER_PID` tables: the low 16 bits of the DWORD hold
/// the port in network byte order.
#[must_use]
pub fn port_from_network(dw: u32) -> u16 {
    let b = dw.to_ne_bytes();
    u16::from_be_bytes([b[0], b[1]])
}

/// Processes owning `port` (TCP listening/connected local port, or UDP local port), mapped to
/// their current `ProcKey` through the snapshot. Sorted: TCP listeners first.
#[must_use]
pub fn owners(snap: &Snapshot, sockets: &[Socket], port: u16) -> Vec<(ProcKey, Socket)> {
    let by_pid: std::collections::HashMap<u32, ProcKey> = snap
        .procs
        .iter()
        .map(|p| (p.raw.key.pid, p.raw.key))
        .collect();
    let rank = |s: &Socket| match (s.proto, s.state) {
        (Proto::Tcp, Some(TcpState::Listen)) => 0,
        (Proto::Tcp, _) => 1,
        (Proto::Udp, _) => 2,
    };
    let mut out: Vec<(ProcKey, Socket)> = sockets
        .iter()
        .filter(|s| s.local.port() == port)
        .filter_map(|s| by_pid.get(&s.owner).map(|k| (*k, *s)))
        .collect();
    out.sort_by_key(|(_, s)| rank(s));
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{RawProc, SystemStats};
    use crate::snapshot::build;
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    #[test]
    fn port_from_network_order() {
        // Port 3000 = 0x0BB8, stored big-endian in the first two bytes of the DWORD.
        assert_eq!(
            port_from_network(u32::from_ne_bytes([0x0B, 0xB8, 0, 0])),
            3000
        );
        assert_eq!(
            port_from_network(u32::from_ne_bytes([0x00, 0x50, 0, 0])),
            80
        );
        assert_eq!(
            port_from_network(u32::from_ne_bytes([0xFF, 0xFF, 0xAB, 0xCD])),
            65535
        );
    }

    #[test]
    fn badge_table() {
        assert_eq!(badge("docker-proxy"), Some(Badge::DockerForward));
        assert_eq!(badge("docker-proxy.exe"), Some(Badge::DockerForward));
        assert_eq!(badge("com.docker.backend.exe"), Some(Badge::DockerForward));
        assert_eq!(badge("WSLRelay.exe"), Some(Badge::WslRelay));
        assert_eq!(badge("node.exe"), None);
        assert_eq!(badge("docker.exe"), None);
    }

    fn sock(proto: Proto, ip: IpAddr, port: u16, state: Option<TcpState>, owner: u32) -> Socket {
        Socket {
            proto,
            local: SocketAddr::new(ip, port),
            remote: None,
            state,
            owner,
        }
    }

    #[test]
    fn owners_maps_pid_to_key_and_puts_listeners_first() {
        let k = |pid| ProcKey {
            pid,
            create_time: u64::from(pid),
        };
        let snap = build(
            None,
            vec![
                RawProc::new(k(10), 0, "node.exe"),
                RawProc::new(k(20), 0, "curl.exe"),
            ],
            SystemStats::default(),
        );
        let v4 = IpAddr::V4(Ipv4Addr::LOCALHOST);
        let v6 = IpAddr::V6(Ipv6Addr::LOCALHOST);
        let sockets = [
            sock(Proto::Tcp, v4, 3000, Some(TcpState::Established), 20),
            sock(Proto::Udp, v6, 3000, None, 10),
            sock(Proto::Tcp, v6, 3000, Some(TcpState::Listen), 10),
            sock(Proto::Tcp, v4, 8080, Some(TcpState::Listen), 10),
            sock(Proto::Tcp, v4, 3000, Some(TcpState::Listen), 99), // PID not in the snapshot
        ];
        let got = owners(&snap, &sockets, 3000);
        let summary: Vec<(u32, Proto, Option<TcpState>)> =
            got.iter().map(|(k, s)| (k.pid, s.proto, s.state)).collect();
        assert_eq!(
            summary,
            vec![
                (10, Proto::Tcp, Some(TcpState::Listen)),
                (20, Proto::Tcp, Some(TcpState::Established)),
                (10, Proto::Udp, None),
            ]
        );
    }

    #[test]
    fn no_sockets_no_owners() {
        assert_eq!(owners(&Snapshot::default(), &[], 3000), vec![]);
    }
}
