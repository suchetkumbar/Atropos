//! Tier-0 scan: one `NtQuerySystemInformation(SystemProcessInformation)` call per tick (spec §3.3).
//! The buffer grows on `STATUS_INFO_LENGTH_MISMATCH` and keeps its size across ticks.

use std::mem::size_of;
use std::sync::Arc;

use windows::Wdk::System::SystemInformation::{NtQuerySystemInformation, SystemProcessInformation};
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::System::Threading::{ALL_PROCESSOR_GROUPS, GetActiveProcessorCount};
use windows::Win32::System::WindowsProgramming::{
    SYSTEM_PROCESS_INFORMATION, SYSTEM_THREAD_INFORMATION,
};

use super::now_filetime;
use crate::platform::{Platform, RawProc, SystemStats};
use crate::{CoreError, ProcKey, Result};

/// `STATUS_INFO_LENGTH_MISMATCH`: the buffer was too small (the process list grew).
#[allow(clippy::cast_possible_wrap)] // NTSTATUS is the bit pattern of a u32
pub(crate) const STATUS_INFO_LENGTH_MISMATCH: i32 = 0xC000_0004_u32 as i32;
/// Initial scan buffer; ~300 processes fit comfortably.
const INITIAL_BYTES: usize = 512 * 1024;
/// Attempts before giving up on a list that keeps growing.
const MAX_ATTEMPTS: u32 = 8;

/// Outcome of one fill attempt.
pub(crate) enum Fill {
    /// Bytes written.
    Done(usize),
    /// Buffer too small; the OS suggests this many bytes (0 = unknown).
    TooSmall(usize),
    /// Any other NTSTATUS.
    Failed(i32),
}

/// Calls `fill` with `buf` (as bytes), growing it on [`Fill::TooSmall`] up to `max_attempts` times.
/// `buf` is `u64`-backed so its bytes are 8-aligned, as `SYSTEM_PROCESS_INFORMATION` requires.
pub(crate) fn grow_retry(
    buf: &mut Vec<u64>,
    max_attempts: u32,
    mut fill: impl FnMut(&mut [u64]) -> Fill,
) -> Result<usize> {
    for _ in 0..max_attempts {
        match fill(buf) {
            Fill::Done(n) => return Ok(n),
            Fill::TooSmall(need) => {
                // Add slack: the list can grow again between this call and the next.
                let want = if need > 0 {
                    need + need / 4
                } else {
                    buf.len() * 16
                };
                buf.resize(want.div_ceil(8).max(buf.len() * 2), 0);
            }
            Fill::Failed(code) => return Err(os_err(code)),
        }
    }
    Err(os_err(STATUS_INFO_LENGTH_MISMATCH))
}

fn os_err(code: i32) -> CoreError {
    CoreError::Os {
        api: "NtQuerySystemInformation",
        code,
    }
}

/// `KTHREAD_STATE::Waiting` and `KWAIT_REASON::Suspended`.
const THREAD_WAITING: u32 = 5;
const WAIT_SUSPENDED: u32 = 5;

/// Little-endian u64 at `at` inside `SYSTEM_PROCESS_INFORMATION::Reserved1`.
fn r1_u64(r1: &[u8; 48], at: usize) -> u64 {
    let mut b = [0u8; 8];
    b.copy_from_slice(&r1[at..at + 8]);
    u64::from_le_bytes(b)
}

fn to_u32(v: usize) -> u32 {
    u32::try_from(v).unwrap_or(u32::MAX)
}

/// Walks the `NextEntryOffset` chain in the first `len` bytes of `buf`, appending to `out`.
///
/// Fields hidden in `Reserved*` are read at the documented NT offsets (phnt `ntexapi.h`):
/// - `Reserved1`: `WorkingSetPrivateSize` @0, `CreateTime` @24, `UserTime` @32, `KernelTime` @40;
/// - `Reserved2`: `InheritedFromUniqueProcessId`;
/// - `Reserved7`: `ReadTransferCount` @\[3\], `WriteTransferCount` @\[4\].
fn parse(buf: &[u64], len: usize, out: &mut Vec<RawProc>) {
    let bytes = (buf.len() * 8).min(len);
    let base = buf.as_ptr().cast::<u8>();
    let (lo, hi) = (base as usize, base as usize + bytes);
    let mut off = 0usize;
    while off + size_of::<SYSTEM_PROCESS_INFORMATION>() <= bytes {
        // SAFETY: the entry lies within the `bytes` the OS filled; read_unaligned has no
        // alignment requirement.
        let e: SYSTEM_PROCESS_INFORMATION =
            unsafe { std::ptr::read_unaligned(base.add(off).cast()) };
        let pid = to_u32(e.UniqueProcessId.0 as usize);

        let name_ptr = e.ImageName.Buffer.0 as usize;
        let name_len = usize::from(e.ImageName.Length);
        let image: Arc<str> = if name_ptr >= lo && name_ptr + name_len <= hi && name_len > 0 {
            // SAFETY: the UTF-16 name lies inside our buffer (bounds checked just above) and
            // `Buffer` is 2-aligned per the UNICODE_STRING contract; read via u16 units.
            let units = unsafe { std::slice::from_raw_parts(name_ptr as *const u16, name_len / 2) };
            Arc::from(String::from_utf16_lossy(units))
        } else if pid == 0 {
            Arc::from("System Idle Process")
        } else {
            Arc::from("")
        };

        let threads_at = off + size_of::<SYSTEM_PROCESS_INFORMATION>();
        let n_threads = e.NumberOfThreads as usize;
        let suspended = n_threads > 0
            && threads_at + n_threads * size_of::<SYSTEM_THREAD_INFORMATION>() <= bytes
            && (0..n_threads).all(|i| {
                // SAFETY: bounds checked above for all `n_threads` entries.
                let t: SYSTEM_THREAD_INFORMATION = unsafe {
                    std::ptr::read_unaligned(
                        base.add(threads_at + i * size_of::<SYSTEM_THREAD_INFORMATION>())
                            .cast(),
                    )
                };
                t.ThreadState == THREAD_WAITING && t.WaitReason == WAIT_SUSPENDED
            });

        out.push(RawProc {
            key: ProcKey {
                pid,
                create_time: r1_u64(&e.Reserved1, 24),
            },
            ppid: to_u32(e.Reserved2 as usize),
            image,
            cpu_100ns: r1_u64(&e.Reserved1, 32).saturating_add(r1_u64(&e.Reserved1, 40)),
            private_ws: r1_u64(&e.Reserved1, 0),
            threads: e.NumberOfThreads,
            handles: e.HandleCount,
            read_bytes: u64::try_from(e.Reserved7[3]).unwrap_or(0),
            write_bytes: u64::try_from(e.Reserved7[4]).unwrap_or(0),
            session: e.SessionId,
            suspended,
        });
        if e.NextEntryOffset == 0 {
            break;
        }
        off += e.NextEntryOffset as usize;
    }
}

fn system_stats() -> SystemStats {
    // SAFETY: plain query with no pointers.
    let logical_cpus = unsafe { GetActiveProcessorCount(ALL_PROCESSOR_GROUPS) };
    let mut mem = MEMORYSTATUSEX {
        dwLength: to_u32(size_of::<MEMORYSTATUSEX>()),
        ..Default::default()
    };
    // SAFETY: `mem` is a valid MEMORYSTATUSEX with dwLength set, as the API requires.
    let (total_ram, used_ram) = match unsafe { GlobalMemoryStatusEx(&raw mut mem) } {
        Ok(()) => (mem.ullTotalPhys, mem.ullTotalPhys - mem.ullAvailPhys),
        Err(_) => (0, 0),
    };
    SystemStats {
        logical_cpus,
        total_ram,
        used_ram,
        now_100ns: now_filetime(),
    }
}

/// The Windows backend.
pub struct WindowsPlatform {
    buf: Vec<u64>,
    sids: super::statics::SidCache,
    /// `atr.exe`, which delivers console Ctrl+C (`atr.exe __ctrl-c <pid> <create_time>`).
    /// `None`: the console strategy is unavailable and stops escalate to force (Z1).
    ctrl_c_helper: Option<std::path::PathBuf>,
    /// Open while GPU figures are shown (`Platform::gpu` … `gpu_stop`).
    gpu: Option<super::gpu::GpuQuery>,
}

impl WindowsPlatform {
    #[must_use]
    pub fn new() -> Self {
        Self {
            buf: vec![0; INITIAL_BYTES / 8],
            sids: super::statics::SidCache::new(),
            ctrl_c_helper: None,
            gpu: None,
        }
    }
}

impl WindowsPlatform {
    /// Uses `helper` (normally `atr.exe`) for console Ctrl+C.
    #[must_use]
    pub fn with_ctrl_c_helper(mut self, helper: std::path::PathBuf) -> Self {
        self.ctrl_c_helper = Some(helper);
        self
    }
}

impl Default for WindowsPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl Platform for WindowsPlatform {
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats> {
        let len = grow_retry(&mut self.buf, MAX_ATTEMPTS, |b| {
            let cap = to_u32(b.len() * 8);
            let mut ret = 0u32;
            // SAFETY: `b` is a writable buffer of `cap` bytes, 8-aligned (u64-backed); `ret` is
            // a valid out-pointer.
            let st = unsafe {
                NtQuerySystemInformation(
                    SystemProcessInformation,
                    b.as_mut_ptr().cast(),
                    cap,
                    &raw mut ret,
                )
            };
            if st.0 == STATUS_INFO_LENGTH_MISMATCH {
                Fill::TooSmall(ret as usize)
            } else if st.is_ok() {
                Fill::Done(ret as usize)
            } else {
                Fill::Failed(st.0)
            }
        })?;
        let stats = system_stats();
        out.clear();
        parse(&self.buf, len, out);
        Ok(stats)
    }

    fn statics(&mut self, key: crate::ProcKey) -> crate::statics::Statics {
        super::statics::read(key, &mut self.sids)
    }

    fn cwd_and_env(&mut self, key: crate::ProcKey) -> crate::Field<crate::platform::ProcEnv> {
        super::peb::read(key, true)
    }

    fn gpu(&mut self) -> Result<Vec<crate::gpu::GpuSample>> {
        let q = match &mut self.gpu {
            Some(q) => q,
            None => self.gpu.insert(super::gpu::GpuQuery::open()?),
        };
        let r = q.sample();
        if r.is_err() {
            // Reopen next time: the counter objects may have gone away (driver reset).
            self.gpu = None;
        }
        r
    }

    fn gpu_stop(&mut self) {
        self.gpu = None;
    }

    fn cwd(&mut self, key: crate::ProcKey) -> crate::Field<std::sync::Arc<str>> {
        super::peb::read(key, false).map(|env| env.cwd)
    }

    fn pin(
        &mut self,
        key: crate::ProcKey,
        access: crate::pin::Access,
    ) -> std::result::Result<crate::pin::Pinned, crate::pin::PinError> {
        super::pin::pin(key, access)
    }

    fn wait_exits(
        &mut self,
        pins: &[&crate::pin::Pinned],
        timeout: std::time::Duration,
    ) -> Vec<bool> {
        super::pin::wait_exits(pins, timeout)
    }

    fn strategy(&mut self, p: &crate::pin::Pinned) -> crate::engine::kill::Strategy {
        super::graceful::strategy(p, self.ctrl_c_helper.as_deref())
    }

    fn graceful(
        &mut self,
        p: &crate::pin::Pinned,
        how: &crate::engine::kill::Strategy,
    ) -> Result<()> {
        super::graceful::graceful(p, how, self.ctrl_c_helper.as_deref())
    }

    fn terminate(&mut self, p: &crate::pin::Pinned) -> Result<()> {
        super::pin::terminate(p)
    }

    fn suspend(&mut self, p: &crate::pin::Pinned) -> Result<()> {
        super::job::suspend(p)
    }

    fn resume(&mut self, p: &crate::pin::Pinned) -> Result<()> {
        super::job::resume(p)
    }

    fn create_job(&mut self) -> Result<crate::pin::Job> {
        super::job::create()
    }

    fn assign(&mut self, job: &crate::pin::Job, p: &crate::pin::Pinned) -> Result<()> {
        super::job::assign(job, p)
    }

    fn terminate_job(&mut self, job: &crate::pin::Job) -> Result<()> {
        super::job::terminate(job)
    }

    fn console_processes(&mut self, key: crate::ProcKey) -> Result<Vec<u32>> {
        super::ctrl_c::console_processes(key, self.ctrl_c_helper.as_deref())
    }

    fn sockets(&mut self) -> Result<Vec<crate::ports::Socket>> {
        super::net::sockets()
    }

    fn services(&mut self) -> Result<Vec<crate::ports::ServiceInfo>> {
        super::services::services()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grow_retry_grows_on_length_mismatch() {
        let mut buf = vec![0u64; 4];
        let mut calls = 0;
        let n = grow_retry(&mut buf, 8, |b| {
            calls += 1;
            if b.len() * 8 < 1000 {
                Fill::TooSmall(1000)
            } else {
                Fill::Done(1000)
            }
        })
        .unwrap();
        assert_eq!(n, 1000);
        assert!(buf.len() * 8 >= 1000);
        assert_eq!(calls, 2);
    }

    #[test]
    fn grow_retry_doubles_when_size_unknown() {
        let mut buf = vec![0u64; 4];
        let n = grow_retry(&mut buf, 8, |b| {
            if b.len() < 16 {
                Fill::TooSmall(0)
            } else {
                Fill::Done(b.len() * 8)
            }
        })
        .unwrap();
        assert_eq!(n, 128);
    }

    #[test]
    fn grow_retry_gives_up_after_max() {
        let mut buf = vec![0u64; 4];
        let mut calls = 0;
        let err = grow_retry(&mut buf, 3, |_| {
            calls += 1;
            Fill::TooSmall(0)
        })
        .unwrap_err();
        assert_eq!(calls, 3);
        assert_eq!(
            err,
            CoreError::Os {
                api: "NtQuerySystemInformation",
                code: STATUS_INFO_LENGTH_MISMATCH
            }
        );
    }

    #[test]
    fn grow_retry_reports_other_status() {
        let mut buf = vec![0u64; 4];
        let err = grow_retry(&mut buf, 3, |_| Fill::Failed(-5)).unwrap_err();
        assert_eq!(
            err,
            CoreError::Os {
                api: "NtQuerySystemInformation",
                code: -5
            }
        );
    }
}
