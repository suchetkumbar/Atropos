//! TCP/UDP tables with owner PIDs (`GetExtendedTcpTable` / `GetExtendedUdpTable`), IPv4 + IPv6.
//! Addresses and ports are in network byte order in these tables.

use std::mem::size_of;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};

use windows::Win32::NetworkManagement::IpHelper::{
    GetExtendedTcpTable, GetExtendedUdpTable, MIB_TCP6ROW_OWNER_PID, MIB_TCPROW_OWNER_PID,
    MIB_UDP6ROW_OWNER_PID, MIB_UDPROW_OWNER_PID, TCP_TABLE_OWNER_PID_ALL, UDP_TABLE_OWNER_PID,
};
use windows::Win32::Networking::WinSock::{AF_INET, AF_INET6};

use crate::ports::{Proto, Socket, TcpState, port_from_network};
use crate::{CoreError, Result};

const NO_ERROR: u32 = 0;
const ERROR_INSUFFICIENT_BUFFER: u32 = 122;
/// `MIB_TCP_STATE_LISTEN` / `MIB_TCP_STATE_ESTAB`.
const TCP_LISTEN: u32 = 2;
const TCP_ESTABLISHED: u32 = 5;

fn tcp_state(s: u32) -> TcpState {
    match s {
        TCP_LISTEN => TcpState::Listen,
        TCP_ESTABLISHED => TcpState::Established,
        other => TcpState::Other(other),
    }
}

/// Fetches one table into an 8-aligned buffer, growing it while the OS says it is too small.
fn fetch(
    api: &'static str,
    mut call: impl FnMut(Option<*mut core::ffi::c_void>, &mut u32) -> u32,
) -> Result<Vec<u64>> {
    let mut size = 0u32;
    let mut buf: Vec<u64> = Vec::new();
    for _ in 0..8 {
        let ptr = (!buf.is_empty()).then(|| buf.as_mut_ptr().cast());
        match call(ptr, &mut size) {
            NO_ERROR if !buf.is_empty() => return Ok(buf),
            NO_ERROR | ERROR_INSUFFICIENT_BUFFER => {
                // The table can grow between calls: add slack.
                buf = vec![0; (size as usize + 4096).div_ceil(8)];
                size = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
            }
            code => {
                return Err(CoreError::Os {
                    api,
                    code: i32::try_from(code).unwrap_or(i32::MAX),
                });
            }
        }
    }
    Err(CoreError::Os {
        api,
        code: i32::try_from(ERROR_INSUFFICIENT_BUFFER).unwrap_or(0),
    })
}

/// Reads the row count (first DWORD) and the `T` rows that follow the 4-byte header, padded
/// to `T`'s alignment.
fn rows<T: Copy>(buf: &[u64]) -> Vec<T> {
    let bytes = buf.len() * 8;
    let base = buf.as_ptr().cast::<u8>();
    // SAFETY: the buffer holds at least the 4-byte count written by the OS (it returned success).
    let n = unsafe { std::ptr::read_unaligned(base.cast::<u32>()) } as usize;
    let first = size_of::<u32>().next_multiple_of(std::mem::align_of::<T>());
    let fits = bytes.saturating_sub(first) / size_of::<T>();
    (0..n.min(fits))
        .map(|i| {
            // SAFETY: row `i` lies within `bytes` (bounded by `fits`); read_unaligned has no
            // alignment requirement.
            unsafe { std::ptr::read_unaligned(base.add(first + i * size_of::<T>()).cast::<T>()) }
        })
        .collect()
}

fn v4(addr: u32, port: u32) -> SocketAddr {
    SocketAddr::new(
        IpAddr::V4(Ipv4Addr::from(addr.to_ne_bytes())),
        port_from_network(port),
    )
}

fn v6(addr: [u8; 16], port: u32) -> SocketAddr {
    SocketAddr::new(IpAddr::V6(Ipv6Addr::from(addr)), port_from_network(port))
}

/// Every TCP and UDP socket, IPv4 and IPv6.
pub(crate) fn sockets() -> Result<Vec<Socket>> {
    let mut out = Vec::new();
    let af4 = u32::from(AF_INET.0);
    let af6 = u32::from(AF_INET6.0);

    // SAFETY (all four calls): `ptr` is `None` or an 8-aligned buffer of `*size` bytes.
    let t4 = fetch("GetExtendedTcpTable", |ptr, size| unsafe {
        GetExtendedTcpTable(ptr, size, false, af4, TCP_TABLE_OWNER_PID_ALL, 0)
    })?;
    for r in rows::<MIB_TCPROW_OWNER_PID>(&t4) {
        let listening = r.dwState == TCP_LISTEN;
        out.push(Socket {
            proto: Proto::Tcp,
            local: v4(r.dwLocalAddr, r.dwLocalPort),
            remote: (!listening).then(|| v4(r.dwRemoteAddr, r.dwRemotePort)),
            state: Some(tcp_state(r.dwState)),
            owner: r.dwOwningPid,
        });
    }
    let t6 = fetch("GetExtendedTcpTable", |ptr, size| unsafe {
        GetExtendedTcpTable(ptr, size, false, af6, TCP_TABLE_OWNER_PID_ALL, 0)
    })?;
    for r in rows::<MIB_TCP6ROW_OWNER_PID>(&t6) {
        let listening = r.dwState == TCP_LISTEN;
        out.push(Socket {
            proto: Proto::Tcp,
            local: v6(r.ucLocalAddr, r.dwLocalPort),
            remote: (!listening).then(|| v6(r.ucRemoteAddr, r.dwRemotePort)),
            state: Some(tcp_state(r.dwState)),
            owner: r.dwOwningPid,
        });
    }
    let u4 = fetch("GetExtendedUdpTable", |ptr, size| unsafe {
        GetExtendedUdpTable(ptr, size, false, af4, UDP_TABLE_OWNER_PID, 0)
    })?;
    for r in rows::<MIB_UDPROW_OWNER_PID>(&u4) {
        out.push(Socket {
            proto: Proto::Udp,
            local: v4(r.dwLocalAddr, r.dwLocalPort),
            remote: None,
            state: None,
            owner: r.dwOwningPid,
        });
    }
    let u6 = fetch("GetExtendedUdpTable", |ptr, size| unsafe {
        GetExtendedUdpTable(ptr, size, false, af6, UDP_TABLE_OWNER_PID, 0)
    })?;
    for r in rows::<MIB_UDP6ROW_OWNER_PID>(&u6) {
        out.push(Socket {
            proto: Proto::Udp,
            local: v6(r.ucLocalAddr, r.dwLocalPort),
            remote: None,
            state: None,
            owner: r.dwOwningPid,
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn v4_address_bytes_are_in_network_order() {
        let a = v4(
            u32::from_ne_bytes([127, 0, 0, 1]),
            u32::from_ne_bytes([0x0B, 0xB8, 0, 0]),
        );
        assert_eq!(a, "127.0.0.1:3000".parse().unwrap());
    }

    #[test]
    fn rows_reads_count_and_padded_rows() {
        // A UDP v4 table: count = 2, then rows of three DWORDs (4-byte alignment, no padding).
        let mut words: Vec<u32> = vec![2, 1, 2, 3, 4, 5, 6];
        words.push(0);
        let buf: Vec<u64> = words
            .chunks(2)
            .map(|c| u64::from(c[0]) | (u64::from(c[1]) << 32))
            .collect();
        let r = rows::<MIB_UDPROW_OWNER_PID>(&buf);
        assert_eq!(r.len(), 2);
        assert_eq!(
            (r[0].dwLocalAddr, r[0].dwLocalPort, r[0].dwOwningPid),
            (1, 2, 3)
        );
        assert_eq!(
            (r[1].dwLocalAddr, r[1].dwLocalPort, r[1].dwOwningPid),
            (4, 5, 6)
        );
    }

    #[test]
    fn rows_never_reads_past_the_buffer() {
        // Claims 1000 rows but holds one.
        let buf: Vec<u64> = vec![0x3E8 | (1 << 32), 0x2 | (3 << 32)];
        assert_eq!(rows::<MIB_UDPROW_OWNER_PID>(&buf).len(), 1);
    }
}
