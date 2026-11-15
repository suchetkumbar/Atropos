//! macOS backend (spec Phase 7, roadmap M7.3): `libproc` for reading, `sysctl(KERN_PROCARGS2)`
//! for the command line and environment, `kqueue` `EVFILT_PROC` for identity and exits.
//!
//! - Identity: `ProcKey.create_time` is `proc_bsdinfo.pbi_start_tvsec/usec` on the Windows
//!   FILETIME epoch.
//! - macOS has no pidfd. A pin registers `EVFILT_PROC` / `NOTE_EXIT` for the PID on its own
//!   kqueue and then re-checks the start time. A PID is only reused after its process exited
//!   (and was reaped), and that exit is queued on the pin's kqueue: every action first checks
//!   the queue and refuses once it reports an exit. Only an exit, reap and full PID wrap
//!   between that check and the `kill(2)` call could slip through (ADR 0009).
//! - Ports and file locks come from `/usr/sbin/lsof -F` (part of macOS; `libc` has no
//!   binding for the socket / vnode `proc_pidfdinfo` structures, ADR 0009).
//! - No job objects, affinity, efficiency mode, services, console siblings or Restart here:
//!   `Unsupported` / empty.

use std::ffi::{CStr, c_int, c_void};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use super::{Platform, ProcEnv, RawProc, SystemStats};
use crate::engine::kill::Strategy;
use crate::pin::{Access, Job, PinError, PinHandle, Pinned};
use crate::ports::{ServiceInfo, Socket};
use crate::procfs::UNIX_EPOCH_FILETIME;
use crate::statics::{Protection, Statics};
use crate::tune::{Priority, Tune, Tuning};
use crate::{CoreError, Field, ProcKey, Result};

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

fn os(api: &'static str) -> CoreError {
    CoreError::Os { api, code: errno() }
}

fn bsdinfo(pid: i32) -> Option<libc::proc_bsdinfo> {
    // SAFETY: `proc_bsdinfo` is plain old data; all-zero is a valid value.
    let mut info: libc::proc_bsdinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>()).ok()?;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    (n == size).then_some(info)
}

fn taskinfo(pid: i32) -> Option<libc::proc_taskinfo> {
    // SAFETY: plain old data; all-zero is valid.
    let mut info: libc::proc_taskinfo = unsafe { std::mem::zeroed() };
    let size = c_int::try_from(std::mem::size_of::<libc::proc_taskinfo>()).ok()?;
    // SAFETY: `info` is a writable buffer of exactly `size` bytes.
    let n = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTASKINFO,
            0,
            (&raw mut info).cast::<c_void>(),
            size,
        )
    };
    (n == size).then_some(info)
}

fn start_of(info: &libc::proc_bsdinfo) -> u64 {
    UNIX_EPOCH_FILETIME + info.pbi_start_tvsec * 10_000_000 + info.pbi_start_tvusec * 10
}

fn key_of(pid: u32) -> Option<ProcKey> {
    let info = bsdinfo(i32::try_from(pid).ok()?)?;
    Some(ProcKey {
        pid,
        create_time: start_of(&info),
    })
}

/// The calling process's key.
///
/// # Errors
/// If `proc_pidinfo` cannot describe our own process.
pub fn current_process_key() -> Result<ProcKey> {
    // SAFETY: no preconditions.
    let pid = unsafe { libc::getpid() };
    key_of(u32::try_from(pid).unwrap_or(0)).ok_or(CoreError::Unsupported)
}

/// `<mach/mach_time.h>` (declared here: the `libc` binding is deprecated in favour of the
/// `mach2` crate, which this one call does not justify).
#[repr(C)]
struct MachTimebase {
    numer: u32,
    denom: u32,
}

unsafe extern "C" {
    fn mach_timebase_info(info: *mut MachTimebase) -> c_int;
}

/// `mach_absolute_time` units → nanoseconds (1/1 on Intel, 125/3 on Apple silicon).
fn timebase() -> (u64, u64) {
    let mut tb = MachTimebase { numer: 1, denom: 1 };
    // SAFETY: `tb` is a valid out pointer.
    if unsafe { mach_timebase_info(&raw mut tb) } != 0 || tb.denom == 0 {
        return (1, 1);
    }
    (u64::from(tb.numer), u64::from(tb.denom))
}

fn pids() -> Vec<i32> {
    // SAFETY: a null buffer asks for the number of PIDs.
    let n = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(n) = usize::try_from(n) else {
        return Vec::new();
    };
    let mut buf = vec![0i32; n + 64];
    let bytes = c_int::try_from(buf.len() * 4).unwrap_or(c_int::MAX);
    // SAFETY: `buf` holds `bytes` writable bytes.
    let got = unsafe { libc::proc_listallpids(buf.as_mut_ptr().cast::<c_void>(), bytes) };
    buf.truncate(usize::try_from(got).unwrap_or(0));
    buf.retain(|p| *p > 0);
    buf
}

fn sysctl_u64(name: &CStr) -> Option<u64> {
    let mut v = 0u64;
    let mut len = std::mem::size_of::<u64>();
    // SAFETY: `name` is NUL-terminated; `v` / `len` describe a writable u64.
    let r = unsafe {
        libc::sysctlbyname(
            name.as_ptr(),
            (&raw mut v).cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    // Some values are 32-bit: `len` says so and the low bytes hold them.
    (r == 0).then_some(if len == 4 { v & 0xFFFF_FFFF } else { v })
}

fn procargs(pid: i32) -> std::result::Result<Vec<u8>, i32> {
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut len = 0usize;
    // SAFETY: `mib` has 3 entries; a null buffer asks for the size.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return Err(errno());
    }
    let mut buf = vec![0u8; len];
    // SAFETY: `buf` holds `len` writable bytes.
    let r = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buf.as_mut_ptr().cast::<c_void>(),
            &raw mut len,
            std::ptr::null_mut(),
            0,
        )
    };
    if r != 0 {
        return Err(errno());
    }
    buf.truncate(len);
    Ok(buf)
}

fn reason<T>(code: i32) -> Field<T> {
    match code {
        libc::EPERM | libc::EACCES | libc::EINVAL => Field::Restricted,
        libc::ESRCH => Field::Gone,
        _ => Field::Unsupported,
    }
}

fn user_name(uid: u32) -> Option<String> {
    // SAFETY: plain old data; all-zero is valid.
    let mut pw: libc::passwd = unsafe { std::mem::zeroed() };
    let mut buf = vec![0 as libc::c_char; 4096];
    let mut out: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: `pw`, `buf` (with its length) and `out` are valid for the call.
    let r =
        unsafe { libc::getpwuid_r(uid, &raw mut pw, buf.as_mut_ptr(), buf.len(), &raw mut out) };
    if r != 0 || out.is_null() || pw.pw_name.is_null() {
        return None;
    }
    // SAFETY: on success `pw_name` points to a NUL-terminated string inside `buf`.
    Some(
        unsafe { CStr::from_ptr(pw.pw_name) }
            .to_string_lossy()
            .into_owned(),
    )
}

fn c_text(bytes: &[libc::c_char]) -> String {
    let b: Vec<u8> = bytes
        .iter()
        .take_while(|c| **c != 0)
        .map(|c| c.to_ne_bytes()[0])
        .collect();
    String::from_utf8_lossy(&b).into_owned()
}

fn lsof(args: &[&std::ffi::OsStr]) -> Result<String> {
    let out = std::process::Command::new("/usr/sbin/lsof")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| CoreError::Unsupported)?;
    // Exit 1 = nothing matched.
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// A pin's kqueue (watching `NOTE_EXIT`) and whether the exit was already seen: `kevent`
/// hands a process-exit event out once, so the pin remembers it.
#[derive(Debug)]
pub struct MacPin {
    fd: OwnedFd,
    gone: AtomicBool,
}

impl MacPin {
    /// Whether the process has exited (checked without waiting).
    fn exited(&self) -> bool {
        if self.gone.load(Ordering::Acquire) {
            return true;
        }
        // SAFETY: plain old data; all-zero is valid.
        let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
        let zero = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: `fd` is a live kqueue; `ev` receives at most one event; zero timeout.
        let n = unsafe {
            libc::kevent(
                self.fd.as_raw_fd(),
                std::ptr::null(),
                0,
                &raw mut ev,
                1,
                &raw const zero,
            )
        };
        if n > 0 {
            self.gone.store(true, Ordering::Release);
        }
        n > 0
    }
}

fn kq(p: &Pinned) -> Result<&MacPin> {
    match &p.handle {
        PinHandle::Mac(m) => Ok(m),
        PinHandle::Fake => Err(CoreError::Unsupported),
    }
}

fn pid_of(p: &Pinned) -> Result<i32> {
    i32::try_from(p.key().pid).map_err(|_| CoreError::Unsupported)
}

/// `kill(2)` after the kqueue check (see the module docs).
fn signal(p: &Pinned, sig: c_int) -> Result<()> {
    if kq(p)?.exited() {
        return Err(CoreError::NotFound);
    }
    // SAFETY: plain call; the pin's kqueue showed no exit, so the PID is still the pinned one.
    if unsafe { libc::kill(pid_of(p)?, sig) } != 0 {
        return Err(os("kill"));
    }
    Ok(())
}

fn nice_of(p: Priority) -> Option<c_int> {
    match p {
        Priority::Idle => Some(19),
        Priority::BelowNormal => Some(10),
        Priority::Normal => Some(0),
        Priority::AboveNormal => Some(-5),
        Priority::High => Some(-10),
        Priority::Realtime => None,
    }
}

fn priority_of(nice: c_int) -> Priority {
    match nice {
        15.. => Priority::Idle,
        5..=14 => Priority::BelowNormal,
        -2..=4 => Priority::Normal,
        -7..=-3 => Priority::AboveNormal,
        _ => Priority::High,
    }
}

/// The macOS backend.
#[derive(Debug)]
pub struct MacPlatform {
    timebase: (u64, u64),
}

impl Default for MacPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl MacPlatform {
    #[must_use]
    pub fn new() -> Self {
        Self {
            timebase: timebase(),
        }
    }

    fn current(key: ProcKey) -> bool {
        key_of(key.pid) == Some(key)
    }
}

impl Platform for MacPlatform {
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats> {
        out.clear();
        let (numer, denom) = self.timebase;
        for pid in pids() {
            let Some(b) = bsdinfo(pid) else {
                continue;
            };
            // Other users' task info is refused: their CPU and memory read as 0.
            let t = taskinfo(pid);
            let ticks = t.map_or(0, |t| t.pti_total_user + t.pti_total_system);
            let mut raw = RawProc::new(
                ProcKey {
                    pid: b.pbi_pid,
                    create_time: start_of(&b),
                },
                b.pbi_ppid,
                &c_text(&b.pbi_comm),
            );
            raw.cpu_100ns = ticks.saturating_mul(numer) / denom / 100;
            raw.private_ws = t.map_or(0, |t| t.pti_resident_size);
            raw.threads = t.map_or(0, |t| u32::try_from(t.pti_threadnum).unwrap_or(0));
            raw.handles = b.pbi_nfiles;
            raw.suspended = b.pbi_status == libc::SSTOP;
            out.push(raw);
        }
        let total = sysctl_u64(c"hw.memsize").unwrap_or(0);
        let page = sysctl_u64(c"hw.pagesize").unwrap_or(4096);
        let free = sysctl_u64(c"vm.page_free_count").unwrap_or(0);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default();
        Ok(SystemStats {
            logical_cpus: std::thread::available_parallelism()
                .map_or(1, |n| u32::try_from(n.get()).unwrap_or(1)),
            total_ram: total,
            used_ram: total.saturating_sub(free.saturating_mul(page)),
            now_100ns: UNIX_EPOCH_FILETIME + u64::try_from(now.as_nanos() / 100).unwrap_or(0),
        })
    }

    fn statics(&mut self, key: ProcKey) -> Statics {
        let Some(b) = i32::try_from(key.pid).ok().and_then(bsdinfo) else {
            return Statics::unavailable(&Field::Gone);
        };
        if start_of(&b) != key.create_time {
            return Statics::unavailable(&Field::Gone);
        }
        let Ok(pid) = i32::try_from(key.pid) else {
            return Statics::unavailable(&Field::Gone);
        };
        let mut path = vec![0u8; 4096];
        // SAFETY: `path` holds 4096 writable bytes (PROC_PIDPATHINFO_MAXSIZE).
        let n = unsafe { libc::proc_pidpath(pid, path.as_mut_ptr().cast::<c_void>(), 4096) };
        let exe_path = if n > 0 {
            path.truncate(usize::try_from(n).unwrap_or(0));
            Field::Ok(Arc::from(String::from_utf8_lossy(&path).as_ref()))
        } else {
            reason(errno())
        };
        let cmdline = match procargs(pid) {
            Ok(buf) => crate::macos_text::parse_procargs2(&buf).map_or(Field::Unsupported, |a| {
                let shown: Vec<String> = a
                    .argv
                    .iter()
                    .map(|s| {
                        if s.contains(char::is_whitespace) {
                            format!("\"{s}\"")
                        } else {
                            s.clone()
                        }
                    })
                    .collect();
                Field::Ok(Arc::from(shown.join(" ").as_str()))
            }),
            Err(code) => reason(code),
        };
        let user = user_name(b.pbi_uid)
            .map_or(Field::Ok(Arc::from(b.pbi_uid.to_string().as_str())), |n| {
                Field::Ok(Arc::from(n.as_str()))
            });
        Statics {
            exe_path,
            cmdline,
            user,
            wow64: Field::Unsupported,
            protection: Field::Ok(Protection::None),
        }
    }

    fn cwd_and_env(&mut self, key: ProcKey) -> Field<ProcEnv> {
        if !Self::current(key) {
            return Field::Gone;
        }
        let Ok(pid) = i32::try_from(key.pid) else {
            return Field::Gone;
        };
        // SAFETY: plain old data; all-zero is valid.
        let mut vp: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
        let size = c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).unwrap_or(0);
        // SAFETY: `vp` is a writable buffer of exactly `size` bytes.
        let n = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDVNODEPATHINFO,
                0,
                (&raw mut vp).cast::<c_void>(),
                size,
            )
        };
        if n != size {
            return reason(errno());
        }
        let cwd = c_text(vp.pvi_cdir.vip_path.as_flattened());
        let env = match procargs(pid) {
            Ok(buf) => crate::macos_text::parse_procargs2(&buf).map_or_else(Vec::new, |a| a.env),
            Err(code) => return reason(code),
        };
        Field::Ok(ProcEnv {
            cwd: Arc::from(format!("{}/", cwd.trim_end_matches('/')).as_str()),
            vars: env,
        })
    }

    fn sockets(&mut self) -> Result<Vec<Socket>> {
        let text = lsof(&[
            "-nP".as_ref(),
            "-iTCP".as_ref(),
            "-iUDP".as_ref(),
            "-F".as_ref(),
            "pPnT".as_ref(),
        ])?;
        Ok(crate::macos_text::parse_lsof_sockets(&text))
    }

    fn file_lockers(&mut self, path: &Path) -> Result<Vec<ProcKey>> {
        let path: PathBuf = path.to_path_buf();
        crate::locks::bounded(crate::locks::TIMEOUT, move || {
            let meta = std::fs::metadata(&path).map_err(|_| CoreError::NotFound)?;
            let flag: &std::ffi::OsStr = if meta.is_dir() {
                "+D".as_ref()
            } else {
                "--".as_ref()
            };
            let text = lsof(&["-F".as_ref(), "p".as_ref(), flag, path.as_os_str()])?;
            Ok(crate::macos_text::parse_lsof_pids(&text)
                .into_iter()
                .filter_map(key_of)
                .collect())
        })
    }

    fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        Ok(Vec::new())
    }

    fn pin(&mut self, key: ProcKey, access: Access) -> std::result::Result<Pinned, PinError> {
        let pid = i32::try_from(key.pid).map_err(|_| PinError::Gone)?;
        // SAFETY: no preconditions.
        let raw = unsafe { libc::kqueue() };
        if raw < 0 {
            return Err(PinError::Os(os("kqueue")));
        }
        // SAFETY: `raw` is a new kqueue descriptor nobody else owns.
        let fd = unsafe { OwnedFd::from_raw_fd(raw) };
        // SAFETY: plain old data; all-zero is valid.
        let mut ev: libc::kevent = unsafe { std::mem::zeroed() };
        ev.ident = usize::try_from(pid).map_err(|_| PinError::Gone)?;
        ev.filter = libc::EVFILT_PROC;
        ev.flags = libc::EV_ADD;
        ev.fflags = libc::NOTE_EXIT;
        // SAFETY: `fd` is a live kqueue; one change, no events, no timeout needed.
        let r = unsafe {
            libc::kevent(
                fd.as_raw_fd(),
                &raw const ev,
                1,
                std::ptr::null_mut(),
                0,
                std::ptr::null(),
            )
        };
        if r < 0 {
            return Err(if errno() == libc::ESRCH {
                PinError::Gone
            } else {
                PinError::Os(os("kevent"))
            });
        }
        // Checked after the watch exists: a PID reused before it shows another start time.
        let Some(b) = bsdinfo(pid) else {
            return Err(PinError::Gone);
        };
        if start_of(&b) != key.create_time {
            return Err(PinError::Reused);
        }
        if access.terminate || access.suspend || access.tune {
            // SAFETY: no preconditions.
            let me = unsafe { libc::geteuid() };
            if me != 0 && b.pbi_uid != me {
                return Err(PinError::Denied);
            }
        }
        Ok(Pinned::new(
            key,
            access,
            PinHandle::Mac(MacPin {
                fd,
                gone: AtomicBool::new(false),
            }),
        ))
    }

    fn wait_exits(&mut self, pins: &[&Pinned], timeout: Duration) -> Vec<bool> {
        let fds: Vec<Option<&MacPin>> = pins.iter().map(|p| kq(p).ok()).collect();
        if fds.iter().flatten().any(|m| m.exited()) {
            return fds.iter().map(|m| m.is_some_and(MacPin::exited)).collect();
        }
        let mut polls: Vec<libc::pollfd> = fds
            .iter()
            .flatten()
            .map(|m| libc::pollfd {
                fd: m.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        if !polls.is_empty() {
            let ms = c_int::try_from(timeout.as_millis()).unwrap_or(c_int::MAX);
            // SAFETY: `polls` is a valid array of `len` pollfd entries.
            let _ = unsafe {
                libc::poll(
                    polls.as_mut_ptr(),
                    libc::nfds_t::try_from(polls.len()).unwrap_or(0),
                    ms,
                )
            };
        }
        fds.iter().map(|m| m.is_some_and(MacPin::exited)).collect()
    }

    fn strategy(&mut self, _p: &Pinned) -> Strategy {
        Strategy::Signal
    }

    fn graceful(&mut self, p: &Pinned, how: &Strategy) -> Result<()> {
        match how {
            Strategy::Signal => signal(p, libc::SIGTERM),
            _ => Err(CoreError::Unsupported),
        }
    }

    fn terminate(&mut self, p: &Pinned) -> Result<()> {
        signal(p, libc::SIGKILL)
    }

    fn suspend(&mut self, p: &Pinned) -> Result<()> {
        signal(p, libc::SIGSTOP)
    }

    fn resume(&mut self, p: &Pinned) -> Result<()> {
        signal(p, libc::SIGCONT)
    }

    fn create_job(&mut self) -> Result<Job> {
        Err(CoreError::Unsupported)
    }

    fn assign(&mut self, _job: &Job, _p: &Pinned) -> Result<()> {
        Err(CoreError::Unsupported)
    }

    fn terminate_job(&mut self, _job: &Job) -> Result<()> {
        Err(CoreError::Unsupported)
    }

    fn console_processes(&mut self, _key: ProcKey) -> Result<Vec<u32>> {
        Err(CoreError::Unsupported)
    }

    fn tune(&mut self, p: &Pinned, t: Tune) -> Result<()> {
        if kq(p)?.exited() {
            return Err(CoreError::NotFound);
        }
        let pid = pid_of(p)?;
        match t {
            Tune::Priority(pr) => {
                let nice = nice_of(pr).ok_or(CoreError::Unsupported)?;
                let who = libc::id_t::try_from(pid).map_err(|_| CoreError::Unsupported)?;
                // SAFETY: plain call on the pinned PID (checked above).
                if unsafe { libc::setpriority(libc::PRIO_PROCESS, who, nice) } != 0 {
                    return Err(os("setpriority"));
                }
                Ok(())
            }
            Tune::Affinity(_) | Tune::Eco(_) => Err(CoreError::Unsupported),
        }
    }

    fn tuning(&mut self, p: &Pinned) -> Result<Tuning> {
        let pid = pid_of(p)?;
        let b = bsdinfo(pid).ok_or(CoreError::NotFound)?;
        let cpus = std::thread::available_parallelism().map_or(1, std::num::NonZero::get);
        let all = if cpus >= 64 {
            u64::MAX
        } else {
            (1u64 << cpus) - 1
        };
        Ok(Tuning {
            priority: priority_of(b.pbi_nice),
            affinity: all,
            system_affinity: all,
            eco: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn our_own_key_is_stable_and_in_the_scan() {
        let a = current_process_key().unwrap();
        assert_eq!(a, current_process_key().unwrap());
        let mut raw = Vec::new();
        MacPlatform::new().scan(&mut raw).unwrap();
        assert!(raw.iter().any(|p| p.key == a));
    }

    #[test]
    fn nice_maps_onto_priority_classes_both_ways() {
        for p in Priority::SETTABLE {
            assert_eq!(priority_of(nice_of(p).unwrap()), p);
        }
    }
}
