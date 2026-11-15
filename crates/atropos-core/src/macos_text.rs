//! Parsers for macOS process data (spec Phase 7): pure, so they are tested on every OS. The
//! macOS backend (`platform/macos`) hands them raw bytes and text.
//!
//! - `sysctl(KERN_PROCARGS2)`: `argc` (native-endian i32), the exec path, NUL padding, then
//!   `argc` NUL-terminated arguments, then `NAME=value` environment strings.
//! - `lsof -F` field output (ports and file locks; `libc` has no binding for the socket and
//!   vnode `proc_pidfdinfo` structures, ADR 0009): one field per line, the first character
//!   naming it (`p` PID, `P` protocol, `n` name, `T` TCP info such as `TST=LISTEN`).

use std::net::SocketAddr;

use crate::ports::{Proto, Socket, TcpState};

/// What `KERN_PROCARGS2` holds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcArgs {
    pub exe: String,
    pub argv: Vec<String>,
    pub env: Vec<(String, String)>,
}

/// Parses a `KERN_PROCARGS2` buffer.
#[must_use]
pub fn parse_procargs2(buf: &[u8]) -> Option<ProcArgs> {
    let count = usize::try_from(i32::from_ne_bytes(buf.get(..4)?.try_into().ok()?)).ok()?;
    let rest = buf.get(4..)?;
    let exe_end = rest.iter().position(|b| *b == 0)?;
    let exe = String::from_utf8_lossy(&rest[..exe_end]).into_owned();
    let mut strings = rest[exe_end..]
        .split(|b| *b == 0)
        .filter(|s| !s.is_empty())
        .map(|s| String::from_utf8_lossy(s).into_owned());
    let argv: Vec<String> = strings.by_ref().take(count).collect();
    if argv.len() != count {
        return None;
    }
    let env = strings
        .filter_map(|s| {
            let (k, v) = s.split_once('=')?;
            (!k.is_empty()).then(|| (k.to_owned(), v.to_owned()))
        })
        .collect();
    Some(ProcArgs { exe, argv, env })
}

/// `127.0.0.1:3000`, `[::1]:3000` or `*:5353` (the wildcard is shown as IPv4).
fn addr(s: &str) -> Option<SocketAddr> {
    if let Some(port) = s.strip_prefix("*:") {
        return Some(SocketAddr::from(([0, 0, 0, 0], port.parse().ok()?)));
    }
    s.parse().ok()
}

/// One `f…` record being collected.
#[derive(Default)]
struct Rec {
    proto: Option<Proto>,
    name: Option<String>,
    state: Option<TcpState>,
}

fn flush(pid: Option<u32>, r: &mut Rec, out: &mut Vec<Socket>) {
    let r = std::mem::take(r);
    let (Some(owner), Some(proto), Some(name)) = (pid, r.proto, r.name) else {
        return;
    };
    let (local, remote) = match name.split_once("->") {
        Some((l, rem)) => (addr(l), addr(rem)),
        None => (addr(&name), None),
    };
    let Some(local) = local else {
        return;
    };
    out.push(Socket {
        proto,
        local,
        remote,
        state: match proto {
            Proto::Tcp => r.state,
            Proto::Udp => None,
        },
        owner,
    });
}

/// Sockets from `lsof -nP -iTCP -iUDP -F pPnT` output.
#[must_use]
pub fn parse_lsof_sockets(text: &str) -> Vec<Socket> {
    let mut out = Vec::new();
    let mut pid = None;
    let mut rec = Rec::default();
    for line in text.lines() {
        let Some(tag) = line.chars().next() else {
            continue;
        };
        let value = &line[tag.len_utf8()..];
        match tag {
            'p' => {
                flush(pid, &mut rec, &mut out);
                pid = value.parse().ok();
            }
            'f' => flush(pid, &mut rec, &mut out),
            'P' => {
                rec.proto = match value {
                    "TCP" => Some(Proto::Tcp),
                    "UDP" => Some(Proto::Udp),
                    _ => None,
                };
            }
            'n' => rec.name = Some(value.to_owned()),
            'T' => {
                if let Some(st) = value.strip_prefix("ST=") {
                    rec.state = Some(match st {
                        "LISTEN" => TcpState::Listen,
                        "ESTABLISHED" => TcpState::Established,
                        _ => TcpState::Other(0),
                    });
                }
            }
            _ => {}
        }
    }
    flush(pid, &mut rec, &mut out);
    out
}

/// PIDs from `lsof -F p` output (file locks), sorted, without duplicates.
#[must_use]
pub fn parse_lsof_pids(text: &str) -> Vec<u32> {
    let mut pids: Vec<u32> = text
        .lines()
        .filter_map(|l| l.strip_prefix('p')?.parse().ok())
        .collect();
    pids.sort_unstable();
    pids.dedup();
    pids
}

#[cfg(test)]
mod tests {
    use super::*;

    fn procargs(argc: i32, parts: &[&str]) -> Vec<u8> {
        let mut b = argc.to_ne_bytes().to_vec();
        for (i, p) in parts.iter().enumerate() {
            b.extend_from_slice(p.as_bytes());
            b.push(0);
            if i == 0 {
                b.extend_from_slice(&[0, 0, 0]); // padding after the exec path
            }
        }
        b
    }

    #[test]
    fn procargs2_splits_exe_arguments_and_environment() {
        let buf = procargs(
            3,
            &[
                "/usr/local/bin/node",
                "node",
                "server.js",
                "--name=my app",
                "PATH=/usr/bin",
                "A=x=y",
            ],
        );
        assert_eq!(
            parse_procargs2(&buf),
            Some(ProcArgs {
                exe: "/usr/local/bin/node".into(),
                argv: vec!["node".into(), "server.js".into(), "--name=my app".into()],
                env: vec![
                    ("PATH".into(), "/usr/bin".into()),
                    ("A".into(), "x=y".into())
                ],
            })
        );
        assert_eq!(parse_procargs2(&[1, 0]), None);
        assert_eq!(parse_procargs2(&procargs(5, &["/bin/x", "x"])), None);
    }

    #[test]
    fn lsof_sockets_parse_listeners_connections_and_udp() {
        let text = "p501\nf4\nPTCP\nn127.0.0.1:3000\nTST=LISTEN\nf5\nPTCP\nn[::1]:3000\nTST=LISTEN\n\
                    p77\nf9\nPTCP\nn127.0.0.1:55000->127.0.0.1:3000\nTST=ESTABLISHED\n\
                    f10\nPUDP\nn*:5353\n";
        let s = parse_lsof_sockets(text);
        assert_eq!(s.len(), 4);
        assert_eq!(s[0].owner, 501);
        assert_eq!(s[0].local, "127.0.0.1:3000".parse().unwrap());
        assert_eq!(s[0].state, Some(TcpState::Listen));
        assert_eq!(s[1].local, "[::1]:3000".parse().unwrap());
        assert_eq!(s[2].owner, 77);
        assert_eq!(s[2].remote, Some("127.0.0.1:3000".parse().unwrap()));
        assert_eq!(s[2].state, Some(TcpState::Established));
        assert_eq!(s[3].proto, Proto::Udp);
        assert_eq!(s[3].local, "0.0.0.0:5353".parse().unwrap());
        assert_eq!(s[3].state, None);
    }

    #[test]
    fn lsof_pids_are_deduplicated() {
        assert_eq!(parse_lsof_pids("p9\nf3\np2\nf1\np9\n"), vec![2, 9]);
        // A non-ASCII first character must not panic (lines are split on chars).
        assert_eq!(parse_lsof_sockets("p1\nf2\nPTCP\nnü:1\nötag\n"), Vec::new());
        assert_eq!(parse_lsof_pids(""), Vec::<u32>::new());
    }
}
