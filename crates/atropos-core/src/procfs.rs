//! Parsers for Linux procfs text (spec Phase 7): pure, so they are tested on every OS. The
//! Linux backend (`platform/linux`) reads the files and hands the text here. Formats follow
//! `proc(5)`.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use crate::ports::{Proto, TcpState};

/// FILETIME ticks (100 ns since 1601) at the Unix epoch, so Linux times share the
/// [`crate::ProcKey::create_time`] epoch with Windows.
pub const UNIX_EPOCH_FILETIME: u64 = 116_444_736_000_000_000;

/// The fields of `/proc/<pid>/stat` the scan uses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Stat {
    pub comm: String,
    /// `R`, `S`, `D`, `T` (stopped), `t`, `Z`, …
    pub state: char,
    pub ppid: u32,
    pub session: u32,
    /// User + system time, clock ticks.
    pub cpu_ticks: u64,
    pub threads: u32,
    /// Clock ticks after boot.
    pub start_ticks: u64,
}

/// Parses one `/proc/<pid>/stat` line. `comm` may contain spaces and `)`: the fields after
/// the **last** `)` are positional.
#[must_use]
pub fn parse_stat(line: &str) -> Option<Stat> {
    let open = line.find('(')?;
    let close = line.rfind(')')?;
    let comm = line.get(open + 1..close)?.to_owned();
    let f: Vec<&str> = line.get(close + 1..)?.split_whitespace().collect();
    // f[0] is field 3 (state); field n is f[n - 3].
    let num = |n: usize| f.get(n - 3).and_then(|v| v.parse::<u64>().ok());
    Some(Stat {
        comm,
        state: f.first()?.chars().next()?,
        ppid: u32::try_from(num(4)?).ok()?,
        session: u32::try_from(num(6)?).ok()?,
        cpu_ticks: num(14)? + num(15)?,
        threads: u32::try_from(num(20)?).ok()?,
        start_ticks: num(22)?,
    })
}

/// `create_time` (FILETIME epoch) of a process that started `start_ticks` after a boot at
/// `btime` (seconds since the Unix epoch, `/proc/stat`).
#[must_use]
pub fn start_filetime(btime: u64, start_ticks: u64, clk_tck: u64) -> u64 {
    UNIX_EPOCH_FILETIME + btime * 10_000_000 + ticks_to_100ns(start_ticks, clk_tck)
}

/// Clock ticks → 100 ns ticks.
#[must_use]
pub fn ticks_to_100ns(ticks: u64, clk_tck: u64) -> u64 {
    ticks.saturating_mul(10_000_000) / clk_tck.max(1)
}

/// `btime` from `/proc/stat`.
#[must_use]
pub fn parse_btime(proc_stat: &str) -> Option<u64> {
    proc_stat
        .lines()
        .find_map(|l| l.strip_prefix("btime "))
        .and_then(|v| v.trim().parse().ok())
}

/// The number in a `Key:   123 kB` line, in bytes (`kB` lines) or as is.
fn kb_field(text: &str, key: &str) -> Option<u64> {
    let rest = text.lines().find_map(|l| l.strip_prefix(key))?;
    let mut it = rest.split_whitespace();
    let n: u64 = it.next()?.parse().ok()?;
    Some(if it.next() == Some("kB") { n * 1024 } else { n })
}

/// `(MemTotal, MemTotal - MemAvailable)` in bytes from `/proc/meminfo`.
#[must_use]
pub fn parse_meminfo(text: &str) -> Option<(u64, u64)> {
    let total = kb_field(text, "MemTotal:")?;
    let avail = kb_field(text, "MemAvailable:")?;
    Some((total, total.saturating_sub(avail)))
}

/// The real UID and the private resident memory (`RssAnon`, bytes) from `/proc/<pid>/status`.
#[must_use]
pub fn parse_status(text: &str) -> (Option<u32>, Option<u64>) {
    let uid = text
        .lines()
        .find_map(|l| l.strip_prefix("Uid:"))
        .and_then(|r| r.split_whitespace().next())
        .and_then(|v| v.parse().ok());
    (uid, kb_field(text, "RssAnon:"))
}

/// UID → account name from `/etc/passwd`.
#[must_use]
pub fn parse_passwd(text: &str) -> HashMap<u32, String> {
    text.lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split(':');
            let name = f.next()?;
            let uid = f.nth(1)?.parse().ok()?;
            (!name.is_empty()).then(|| (uid, name.to_owned()))
        })
        .collect()
}

/// `/proc/<pid>/cmdline` (NUL-separated) as one display line; arguments with whitespace are
/// quoted.
#[must_use]
pub fn cmdline_text(raw: &[u8]) -> String {
    raw.split(|b| *b == 0)
        .filter(|a| !a.is_empty())
        .map(|a| {
            let s = String::from_utf8_lossy(a);
            if s.contains(char::is_whitespace) {
                format!("\"{s}\"")
            } else {
                s.into_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// `/proc/<pid>/environ` (NUL-separated `NAME=value`).
#[must_use]
pub fn parse_environ(raw: &[u8]) -> Vec<(String, String)> {
    raw.split(|b| *b == 0)
        .filter_map(|e| {
            let s = String::from_utf8_lossy(e);
            let (k, v) = s.split_once('=')?;
            (!k.is_empty()).then(|| (k.to_owned(), v.to_owned()))
        })
        .collect()
}

/// `0100007F:0BB8` or the 32-hex-digit IPv6 form; each 32-bit word is in host (little-endian)
/// order.
fn parse_addr(s: &str) -> Option<SocketAddr> {
    let (ip, port) = s.split_once(':')?;
    let port = u16::from_str_radix(port, 16).ok()?;
    let word = |i: usize| {
        ip.get(i * 8..i * 8 + 8)
            .and_then(|w| u32::from_str_radix(w, 16).ok())
            .map(u32::to_le_bytes)
    };
    let ip = match ip.len() {
        8 => IpAddr::V4(Ipv4Addr::from(word(0)?)),
        32 => {
            let mut b = [0u8; 16];
            for i in 0..4 {
                b[i * 4..i * 4 + 4].copy_from_slice(&word(i)?);
            }
            IpAddr::V6(Ipv6Addr::from(b))
        }
        _ => return None,
    };
    Some(SocketAddr::new(ip, port))
}

/// One socket row of `/proc/net/{tcp,tcp6,udp,udp6}`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetRow {
    pub local: SocketAddr,
    pub remote: Option<SocketAddr>,
    pub state: Option<TcpState>,
    pub inode: u64,
}

/// Parses a `/proc/net/…` table (header line skipped, bad rows skipped).
#[must_use]
pub fn parse_net(text: &str, proto: Proto) -> Vec<NetRow> {
    text.lines()
        .skip(1)
        .filter_map(|l| {
            let f: Vec<&str> = l.split_whitespace().collect();
            let local = parse_addr(f.get(1)?)?;
            let remote = parse_addr(f.get(2)?)?;
            let st = u8::from_str_radix(f.get(3)?, 16).ok()?;
            let inode = f.get(9)?.parse().ok()?;
            let state = match proto {
                Proto::Udp => None,
                Proto::Tcp => Some(match st {
                    0x0A => TcpState::Listen,
                    0x01 => TcpState::Established,
                    other => TcpState::Other(u32::from(other)),
                }),
            };
            Some(NetRow {
                local,
                remote: (remote.port() != 0 || !remote.ip().is_unspecified()).then_some(remote),
                state,
                inode,
            })
        })
        .collect()
}

/// The inode of a `/proc/<pid>/fd/<n>` link target `socket:[12345]`.
#[must_use]
pub fn socket_inode(link: &str) -> Option<u64> {
    link.strip_prefix("socket:[")?
        .strip_suffix(']')?
        .parse()
        .ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stat_lines_with_odd_names_parse() {
        let line = "4242 (my (weird) proc) S 1 4242 4242 0 -1 4194560 100 0 0 0 \
                    250 50 0 0 20 0 7 0 123456 1000000 300 18446744073709551615";
        assert_eq!(
            parse_stat(line),
            Some(Stat {
                comm: "my (weird) proc".into(),
                state: 'S',
                ppid: 1,
                session: 4242,
                cpu_ticks: 300,
                threads: 7,
                start_ticks: 123_456,
            })
        );
        assert_eq!(parse_stat("1 (init"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn times_share_the_windows_epoch() {
        // Boot at 2026-01-01T00:00:00Z, started 2.5 s later with CLK_TCK 100.
        let btime = 1_767_225_600;
        assert_eq!(
            start_filetime(btime, 250, 100),
            UNIX_EPOCH_FILETIME + btime * 10_000_000 + 25_000_000
        );
        assert_eq!(ticks_to_100ns(3, 100), 300_000);
        assert_eq!(
            parse_btime("cpu  1 2 3\nbtime 1767225600\nprocesses 9\n"),
            Some(btime)
        );
    }

    #[test]
    fn meminfo_and_status() {
        let mem = "MemTotal:       16000000 kB\nMemFree:  1 kB\nMemAvailable:    6000000 kB\n";
        assert_eq!(
            parse_meminfo(mem),
            Some((16_000_000 * 1024, 10_000_000 * 1024))
        );
        let status =
            "Name:\tnode\nUid:\t1000\t1000\t1000\t1000\nVmRSS:\t  900 kB\nRssAnon:\t  512 kB\n";
        assert_eq!(parse_status(status), (Some(1000), Some(512 * 1024)));
        assert_eq!(parse_status("Name:\tkthreadd\n"), (None, None));
    }

    #[test]
    fn passwd_maps_uids_to_names() {
        let m = parse_passwd(
            "root:x:0:0:root:/root:/bin/bash\n# comment\njosé:x:1000:1000::/home/josé:/bin/zsh\nbad line\n",
        );
        assert_eq!(m.get(&0).map(String::as_str), Some("root"));
        assert_eq!(m.get(&1000).map(String::as_str), Some("josé"));
        assert_eq!(m.len(), 2);
    }

    #[test]
    fn cmdline_and_environ_split_on_nul() {
        assert_eq!(
            cmdline_text(b"node\0server.js\0--name\0my app\0"),
            r#"node server.js --name "my app""#
        );
        assert_eq!(cmdline_text(b""), "");
        assert_eq!(
            parse_environ(b"PATH=/bin\0A=x=y\0junk\0"),
            vec![("PATH".into(), "/bin".into()), ("A".into(), "x=y".into())]
        );
    }

    #[test]
    fn net_tables_parse_v4_and_v6() {
        let tcp = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 0100007F:0BB8 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0\n\
   1: 0100007F:9C40 0100007F:0BB8 01 00000000:00000000 00:00000000 00000000  1000        0 222 1\n\
 garbage\n";
        let rows = parse_net(tcp, Proto::Tcp);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].local, "127.0.0.1:3000".parse().unwrap());
        assert_eq!(rows[0].state, Some(TcpState::Listen));
        assert_eq!(rows[0].remote, None);
        assert_eq!(rows[0].inode, 12345);
        assert_eq!(rows[1].remote, Some("127.0.0.1:3000".parse().unwrap()));
        assert_eq!(rows[1].state, Some(TcpState::Established));

        let tcp6 = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
   0: 00000000000000000000000001000000:1F90 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 999 1\n";
        let v6 = parse_net(tcp6, Proto::Tcp);
        assert_eq!(v6[0].local, "[::1]:8080".parse().unwrap());

        let udp = "  sl  local_address rem_address   st\n  0: 00000000:14E9 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 77 2\n";
        let u = parse_net(udp, Proto::Udp);
        assert_eq!(u[0].local, "0.0.0.0:5353".parse().unwrap());
        assert_eq!(u[0].state, None);
    }

    #[test]
    fn socket_links_give_their_inode() {
        assert_eq!(socket_inode("socket:[12345]"), Some(12345));
        assert_eq!(socket_inode("/dev/null"), None);
        assert_eq!(socket_inode("pipe:[9]"), None);
    }
}
