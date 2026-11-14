//! Platform abstraction. Each OS backend implements [`Platform`]; `fake` is the scripted test backend.
//!
//! The trait grows one phase at a time toward spec §4.4.

use std::sync::Arc;

use crate::statics::Statics;
use crate::{ProcKey, Result};

pub mod fake;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;

pub use fake::FakePlatform;

/// One process as returned by a single scan (tier 0): identity plus the per-tick dynamic fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProc {
    pub key: ProcKey,
    /// Parent PID as recorded at creation; only trusted after the create-time check (`tree`).
    pub ppid: u32,
    pub image: Arc<str>,
    /// Kernel + user CPU time, 100 ns ticks.
    pub cpu_100ns: u64,
    /// Private working set, bytes (the Task Manager "Memory" column).
    pub private_ws: u64,
    pub threads: u32,
    pub handles: u32,
    /// Cumulative bytes read / written (all I/O, as Task Manager's disk columns approximate).
    pub read_bytes: u64,
    pub write_bytes: u64,
    pub session: u32,
    /// Every thread is waiting with reason `Suspended`.
    pub suspended: bool,
}

impl RawProc {
    /// A process with identity only; every dynamic field zero.
    #[must_use]
    pub fn new(key: ProcKey, ppid: u32, image: &str) -> Self {
        Self {
            key,
            ppid,
            image: Arc::from(image),
            cpu_100ns: 0,
            private_ws: 0,
            threads: 0,
            handles: 0,
            read_bytes: 0,
            write_bytes: 0,
            session: 0,
            suspended: false,
        }
    }
}

/// Tier-2 data, read on demand for the selected process only (it needs `PROCESS_VM_READ`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProcEnv {
    /// Current directory, as the process sees it (with a trailing `\`).
    pub cwd: Arc<str>,
    /// Environment variables in block order; `=C:`-style drive entries are omitted.
    pub vars: Vec<(String, String)>,
}

/// Machine-wide figures returned with each scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SystemStats {
    pub logical_cpus: u32,
    /// Physical memory, bytes.
    pub total_ram: u64,
    pub used_ram: u64,
    /// When the scan was taken: FILETIME ticks (100 ns since 1601).
    pub now_100ns: u64,
}

/// An OS backend.
pub trait Platform: Send {
    /// Enumerates all processes. Implementations must `clear()` and refill `out`, reusing its capacity.
    ///
    /// # Errors
    /// Returns an error only when the enumeration itself fails, never for a single unreadable process.
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats>;

    /// Reads the tier-1 statics of `key`. Never fails: unreadable fields become
    /// `Restricted` / `Gone` / `Unsupported`; a PID now owned by a different process is `Gone`.
    fn statics(&mut self, key: ProcKey) -> Statics;

    /// Reads the working directory and environment of `key` (tier 2, on demand).
    fn cwd_and_env(&mut self, key: ProcKey) -> crate::Field<ProcEnv>;
}
