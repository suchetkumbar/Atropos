//! Fixture: binds a loopback socket on an OS-chosen port, prints its key line, then `port=<n>`,
//! and sleeps. `--proto tcp|udp` (default tcp), `--ip 4|6` (default 4), `--secs <n>` (default 3600).

use std::io::Write as _;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener, UdpSocket};
use std::time::Duration;

fn main() -> anyhow::Result<()> {
    let (mut udp, mut v6, mut secs) = (false, false, 3600u64);
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--proto", Some(p)) => udp = p == "udp",
            ("--ip", Some(v)) => v6 = v == "6",
            ("--secs", Some(s)) => secs = s.parse().unwrap_or(secs),
            _ => {}
        }
    }
    let addr: SocketAddr = if v6 {
        (Ipv6Addr::LOCALHOST, 0).into()
    } else {
        (Ipv4Addr::LOCALHOST, 0).into()
    };
    // Keep the socket alive for the whole sleep.
    let (_tcp, _udp, port) = if udp {
        let s = UdpSocket::bind(addr)?;
        let port = s.local_addr()?.port();
        (None, Some(s), port)
    } else {
        let l = TcpListener::bind(addr)?;
        let port = l.local_addr()?.port();
        (Some(l), None, port)
    };
    let key = atropos_core::platform::windows::current_process_key()?;
    let mut out = std::io::stdout().lock();
    writeln!(out, "{}", atropos_fixtures::key_line(key))?;
    writeln!(out, "port={port}")?;
    out.flush()?;
    drop(out);
    std::thread::sleep(Duration::from_secs(secs));
    Ok(())
}
