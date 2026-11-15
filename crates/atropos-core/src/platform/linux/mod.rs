//! Linux backend (spec Phase 7, roadmap M7.2): procfs for reading, **pidfds** for every action.
//!
//! - Identity: `ProcKey.create_time` is the start time from `/proc/<pid>/stat` (clock ticks
//!   after boot) on the Windows FILETIME epoch (`procfs::start_filetime`), so keys look the same
//!   on every OS.
//! - A pin opens a pidfd (`pidfd_open`, Linux 5.3+) and *then* re-reads the start time: if the
//!   PID was reused in between, the start time differs and the pin is `Reused`. Signals go
//!   through `pidfd_send_signal`, so a pinned action can never reach another process.
//! - Graceful = `SIGTERM`, force = `SIGKILL`, suspend / resume = `SIGSTOP` / `SIGCONT`.
//! - No job objects: `create_job` is `Unsupported` and the subtree engine handles members one
//!   by one after freezing them (no new children can appear from a stopped process).
//! - Other users' processes can be read only partly; acting on them is `Denied` (unless root).
//!
//! See `docs/notes/linux-apis.md`.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use rustix::event::{PollFd, PollFlags, poll};
use rustix::fd::OwnedFd;
use rustix::io::Errno;
use rustix::process::{
    Pid, PidfdFlags, Signal, getpriority_process, pidfd_open, pidfd_send_signal,
    setpriority_process,
};
use rustix::thread::{CpuSet, sched_getaffinity, sched_setaffinity};

use super::{Platform, ProcEnv, RawProc, SystemStats};
use crate::engine::kill::Strategy;
use crate::pin::{Access, Job, PinError, PinHandle, Pinned};
use crate::ports::{Proto, ServiceInfo, Socket};
use crate::procfs::{self, Stat};
use crate::statics::{Protection, Statics};
use crate::tune::{Priority, Tune, Tuning};
use crate::{CoreError, Field, ProcKey, Result};

fn os(api: &'static str, e: Errno) -> CoreError {
    CoreError::Os {
        api,
        code: e.raw_os_error(),
    }
}

fn read(path: impl AsRef<Path>) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

fn field_of<T>(e: &std::io::Error) -> Field<T> {
    match e.kind() {
        std::io::ErrorKind::PermissionDenied => Field::Restricted,
        std::io::ErrorKind::NotFound => Field::Gone,
        _ => Field::Unsupported,
    }
}

/// The calling process's key.
///
/// # Errors
/// If `/proc/self/stat` cannot be read (procfs not mounted).
pub fn current_process_key() -> Result<ProcKey> {
    let me = rustix::process::getpid().as_raw_pid();
    LinuxPlatform::new()
        .key_of(u32::try_from(me).unwrap_or(0))
        .ok_or(CoreError::Unsupported)
}

/// The Linux backend.
#[derive(Debug)]
pub struct LinuxPlatform {
    clk_tck: u64,
    btime: u64,
    /// UID → name, read once from `/etc/passwd`.
    users: Option<HashMap<u32, String>>,
}

impl Default for LinuxPlatform {
    fn default() -> Self {
        Self::new()
    }
}

impl LinuxPlatform {
    #[must_use]
    pub fn new() -> Self {
        Self {
            clk_tck: rustix::param::clock_ticks_per_second(),
            btime: read("/proc/stat")
                .ok()
                .and_then(|s| procfs::parse_btime(&s))
                .unwrap_or(0),
            users: None,
        }
    }

    fn stat(pid: u32) -> Option<Stat> {
        procfs::parse_stat(&read(format!("/proc/{pid}/stat")).ok()?)
    }

    fn key_of(&self, pid: u32) -> Option<ProcKey> {
        let s = Self::stat(pid)?;
        (s.state != 'Z' && s.state != 'X').then(|| ProcKey {
            pid,
            create_time: procfs::start_filetime(self.btime, s.start_ticks, self.clk_tck),
        })
    }

    /// Whether `key` is still the process with that PID.
    fn current(&self, key: ProcKey) -> bool {
        self.key_of(key.pid) == Some(key)
    }

    fn user_name(&mut self, uid: u32) -> String {
        let users = self.users.get_or_insert_with(|| {
            read("/etc/passwd")
                .map(|t| procfs::parse_passwd(&t))
                .unwrap_or_default()
        });
        users.get(&uid).cloned().unwrap_or_else(|| uid.to_string())
    }

    /// Every numeric entry of `/proc`.
    fn pids() -> Vec<u32> {
        std::fs::read_dir("/proc")
            .map(|d| {
                d.flatten()
                    .filter_map(|e| e.file_name().to_str()?.parse().ok())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `(pid, link target)` for every readable `/proc/<pid>/fd/<n>`.
    fn fd_links() -> Vec<(u32, PathBuf)> {
        let mut out = Vec::new();
        for pid in Self::pids() {
            let Ok(fds) = std::fs::read_dir(format!("/proc/{pid}/fd")) else {
                continue;
            };
            for fd in fds.flatten() {
                if let Ok(target) = std::fs::read_link(fd.path()) {
                    out.push((pid, target));
                }
            }
        }
        out
    }
}

fn now_100ns() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    procfs::UNIX_EPOCH_FILETIME + u64::try_from(since.as_nanos() / 100).unwrap_or(0)
}

/// `rchar` / `wchar` from `/proc/<pid>/io` (all I/O, like the Windows counters).
fn io_bytes(pid: u32) -> (u64, u64) {
    let Ok(text) = read(format!("/proc/{pid}/io")) else {
        return (0, 0);
    };
    let get = |k: &str| {
        text.lines()
            .find_map(|l| l.strip_prefix(k))
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(0)
    };
    (get("rchar:"), get("wchar:"))
}

fn pidfd(p: &Pinned) -> Result<&OwnedFd> {
    match &p.handle {
        PinHandle::Linux(fd) => Ok(fd),
        PinHandle::Fake => Err(CoreError::Unsupported),
    }
}

fn signal(p: &Pinned, sig: Signal, api: &'static str) -> Result<()> {
    pidfd_send_signal(pidfd(p)?, sig).map_err(|e| os(api, e))
}

/// Whether the pinned process has exited (its pidfd is readable).
fn exited(fd: &OwnedFd) -> bool {
    let mut fds = [PollFd::new(fd, PollFlags::IN)];
    let zero = rustix::event::Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    poll(&mut fds, Some(&zero)).is_ok_and(|n| n > 0)
}

fn raw_pid(key: ProcKey) -> Option<Pid> {
    Pid::from_raw(i32::try_from(key.pid).ok()?)
}

/// Windows priority classes on the nice scale (negative values need `CAP_SYS_NICE`).
fn nice_of(p: Priority) -> Option<i32> {
    match p {
        Priority::Idle => Some(19),
        Priority::BelowNormal => Some(10),
        Priority::Normal => Some(0),
        Priority::AboveNormal => Some(-5),
        Priority::High => Some(-10),
        Priority::Realtime => None,
    }
}

fn priority_of(nice: i32) -> Priority {
    match nice {
        15.. => Priority::Idle,
        5..=14 => Priority::BelowNormal,
        -2..=4 => Priority::Normal,
        -7..=-3 => Priority::AboveNormal,
        _ => Priority::High,
    }
}

fn mask_of(set: &CpuSet) -> u64 {
    (0..64)
        .filter(|&c| set.is_set(c))
        .fold(0, |m, c| m | (1u64 << c))
}

impl Platform for LinuxPlatform {
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats> {
        out.clear();
        for pid in Self::pids() {
            let Some(s) = Self::stat(pid) else {
                continue; // exited while we looked
            };
            if s.state == 'Z' || s.state == 'X' {
                continue;
            }
            let (_, rss) = read(format!("/proc/{pid}/status"))
                .map_or((None, None), |t| procfs::parse_status(&t));
            let (read_bytes, write_bytes) = io_bytes(pid);
            out.push(RawProc {
                key: ProcKey {
                    pid,
                    create_time: procfs::start_filetime(self.btime, s.start_ticks, self.clk_tck),
                },
                ppid: s.ppid,
                image: Arc::from(s.comm.as_str()),
                cpu_100ns: procfs::ticks_to_100ns(s.cpu_ticks, self.clk_tck),
                private_ws: rss.unwrap_or(0),
                threads: s.threads,
                handles: 0,
                read_bytes,
                write_bytes,
                session: s.session,
                suspended: s.state == 'T' || s.state == 't',
            });
        }
        let (total_ram, used_ram) = read("/proc/meminfo")
            .ok()
            .and_then(|t| procfs::parse_meminfo(&t))
            .unwrap_or((0, 0));
        Ok(SystemStats {
            logical_cpus: std::thread::available_parallelism()
                .map_or(1, |n| u32::try_from(n.get()).unwrap_or(1)),
            total_ram,
            used_ram,
            now_100ns: now_100ns(),
        })
    }

    fn statics(&mut self, key: ProcKey) -> Statics {
        if !self.current(key) {
            return Statics::unavailable(&Field::Gone);
        }
        let pid = key.pid;
        let exe_path = match std::fs::read_link(format!("/proc/{pid}/exe")) {
            Ok(p) => Field::Ok(Arc::from(p.display().to_string().as_str())),
            Err(e) => field_of(&e),
        };
        let cmdline = match std::fs::read(format!("/proc/{pid}/cmdline")) {
            Ok(raw) => Field::Ok(Arc::from(procfs::cmdline_text(&raw).as_str())),
            Err(e) => field_of(&e),
        };
        let user = match read(format!("/proc/{pid}/status")) {
            Ok(t) => match procfs::parse_status(&t).0 {
                Some(uid) => Field::Ok(Arc::from(self.user_name(uid).as_str())),
                None => Field::Unsupported,
            },
            Err(e) => field_of(&e),
        };
        Statics {
            exe_path,
            cmdline,
            user,
            wow64: Field::Unsupported,
            protection: Field::Ok(Protection::None),
        }
    }

    fn cwd_and_env(&mut self, key: ProcKey) -> Field<ProcEnv> {
        if !self.current(key) {
            return Field::Gone;
        }
        let pid = key.pid;
        let cwd = match std::fs::read_link(format!("/proc/{pid}/cwd")) {
            Ok(p) => p.display().to_string(),
            Err(e) => return field_of(&e),
        };
        let vars = match std::fs::read(format!("/proc/{pid}/environ")) {
            Ok(raw) => procfs::parse_environ(&raw),
            Err(e) => return field_of(&e),
        };
        Field::Ok(ProcEnv {
            cwd: Arc::from(format!("{}/", cwd.trim_end_matches('/')).as_str()),
            vars,
        })
    }

    fn sockets(&mut self) -> Result<Vec<Socket>> {
        let owners: HashMap<u64, u32> = Self::fd_links()
            .into_iter()
            .filter_map(|(pid, t)| Some((procfs::socket_inode(t.to_str()?)?, pid)))
            .collect();
        let mut out = Vec::new();
        for (file, proto) in [
            ("tcp", Proto::Tcp),
            ("tcp6", Proto::Tcp),
            ("udp", Proto::Udp),
            ("udp6", Proto::Udp),
        ] {
            let Ok(text) = read(format!("/proc/net/{file}")) else {
                continue; // e.g. no IPv6
            };
            for row in procfs::parse_net(&text, proto) {
                // Sockets of processes we cannot look into have no known owner: left out.
                if let Some(&owner) = owners.get(&row.inode) {
                    out.push(Socket {
                        proto,
                        local: row.local,
                        remote: row.remote,
                        state: row.state,
                        owner,
                    });
                }
            }
        }
        Ok(out)
    }

    fn file_lockers(&mut self, path: &Path) -> Result<Vec<ProcKey>> {
        let path = path.to_path_buf();
        let me = Self::new();
        crate::locks::bounded(crate::locks::TIMEOUT, move || {
            let wanted: Vec<PathBuf> = crate::locks::targets(&path, crate::locks::MAX_FILES)?
                .into_iter()
                .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
                .collect();
            let mut keys: Vec<ProcKey> = Self::fd_links()
                .into_iter()
                .filter(|(_, t)| wanted.contains(t))
                .filter_map(|(pid, _)| me.key_of(pid))
                .collect();
            keys.sort_by_key(|k| (k.pid, k.create_time));
            keys.dedup();
            Ok(keys)
        })
    }

    fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        Ok(Vec::new())
    }

    fn pin(&mut self, key: ProcKey, access: Access) -> std::result::Result<Pinned, PinError> {
        let pid = raw_pid(key).ok_or(PinError::Gone)?;
        let fd = match pidfd_open(pid, PidfdFlags::empty()) {
            Ok(fd) => fd,
            Err(Errno::SRCH) => return Err(PinError::Gone),
            Err(e) => return Err(PinError::Os(os("pidfd_open", e))),
        };
        // Checked after the pidfd exists: a PID reused before `pidfd_open` shows another
        // start time here.
        match self.key_of(key.pid) {
            None => return Err(PinError::Gone),
            Some(k) if k != key => return Err(PinError::Reused),
            Some(_) => {}
        }
        if access.terminate || access.suspend || access.tune {
            let uid = read(format!("/proc/{}/status", key.pid))
                .ok()
                .and_then(|t| procfs::parse_status(&t).0);
            let me = rustix::process::geteuid().as_raw();
            if me != 0 && uid != Some(me) {
                return Err(PinError::Denied);
            }
        }
        Ok(Pinned::new(key, access, PinHandle::Linux(fd)))
    }

    fn wait_exits(&mut self, pins: &[&Pinned], timeout: Duration) -> Vec<bool> {
        let fds: Vec<Option<&OwnedFd>> = pins.iter().map(|p| pidfd(p).ok()).collect();
        let mut polls: Vec<PollFd<'_>> = fds
            .iter()
            .flatten()
            .map(|fd| PollFd::new(*fd, PollFlags::IN))
            .collect();
        let ts = rustix::event::Timespec {
            tv_sec: i64::try_from(timeout.as_secs()).unwrap_or(i64::MAX),
            tv_nsec: i64::from(timeout.subsec_nanos()),
        };
        if !polls.is_empty() {
            let _ = poll(&mut polls, Some(&ts));
        }
        fds.iter().map(|fd| fd.is_some_and(exited)).collect()
    }

    fn strategy(&mut self, _p: &Pinned) -> Strategy {
        Strategy::Signal
    }

    fn graceful(&mut self, p: &Pinned, how: &Strategy) -> Result<()> {
        match how {
            Strategy::Signal => signal(p, Signal::TERM, "pidfd_send_signal(SIGTERM)"),
            _ => Err(CoreError::Unsupported),
        }
    }

    fn terminate(&mut self, p: &Pinned) -> Result<()> {
        signal(p, Signal::KILL, "pidfd_send_signal(SIGKILL)")
    }

    fn suspend(&mut self, p: &Pinned) -> Result<()> {
        signal(p, Signal::STOP, "pidfd_send_signal(SIGSTOP)")
    }

    fn resume(&mut self, p: &Pinned) -> Result<()> {
        signal(p, Signal::CONT, "pidfd_send_signal(SIGCONT)")
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
        let fd = pidfd(p)?;
        let pid = raw_pid(p.key()).ok_or(CoreError::Unsupported)?;
        // No pidfd form exists for these calls: the pidfd proves the PID still belongs to
        // the pinned process right before and right after.
        if exited(fd) {
            return Err(CoreError::NotFound);
        }
        let r = match t {
            Tune::Priority(pr) => {
                let nice = nice_of(pr).ok_or(CoreError::Unsupported)?;
                setpriority_process(Some(pid), nice).map_err(|e| os("setpriority", e))
            }
            Tune::Affinity(mask) => {
                let mut set = CpuSet::new();
                for cpu in (0..64).filter(|c| mask & (1u64 << c) != 0) {
                    set.set(cpu);
                }
                sched_setaffinity(Some(pid), &set).map_err(|e| os("sched_setaffinity", e))
            }
            Tune::Eco(_) => Err(CoreError::Unsupported),
        };
        if exited(fd) {
            return Err(CoreError::NotFound);
        }
        r
    }

    fn tuning(&mut self, p: &Pinned) -> Result<Tuning> {
        let fd = pidfd(p)?;
        let pid = raw_pid(p.key()).ok_or(CoreError::Unsupported)?;
        if exited(fd) {
            return Err(CoreError::NotFound);
        }
        let nice = getpriority_process(Some(pid)).map_err(|e| os("getpriority", e))?;
        let affinity =
            mask_of(&sched_getaffinity(Some(pid)).map_err(|e| os("sched_getaffinity", e))?);
        let mine = mask_of(&sched_getaffinity(None).map_err(|e| os("sched_getaffinity", e))?);
        Ok(Tuning {
            priority: priority_of(nice),
            affinity,
            system_affinity: mine,
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
        let b = current_process_key().unwrap();
        assert_eq!(a, b);
        let mut raw = Vec::new();
        LinuxPlatform::new().scan(&mut raw).unwrap();
        assert!(raw.iter().any(|p| p.key == a));
    }

    #[test]
    fn nice_maps_onto_priority_classes_both_ways() {
        for p in Priority::SETTABLE {
            assert_eq!(priority_of(nice_of(p).unwrap()), p);
        }
        assert_eq!(nice_of(Priority::Realtime), None);
    }
}
