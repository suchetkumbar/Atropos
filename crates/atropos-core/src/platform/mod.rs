//! Platform abstraction. Each OS backend implements [`Platform`]; `fake` is the scripted test backend.
//!
//! The trait grows one phase at a time toward spec §4.4.

use std::sync::Arc;

use crate::{ProcKey, Result};

pub mod fake;
#[cfg(windows)]
#[allow(unsafe_code)]
pub mod windows;

pub use fake::FakePlatform;

/// One process as returned by a single scan (tier 0). Extended in Phase 1 with `ProcDyn` fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawProc {
    pub key: ProcKey,
    pub ppid: u32,
    pub image: Arc<str>,
}

/// Machine-wide figures returned with each scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SystemStats {
    pub logical_cpus: u32,
}

/// An OS backend.
pub trait Platform: Send {
    /// Enumerates all processes. Implementations must `clear()` and refill `out`, reusing its capacity.
    ///
    /// # Errors
    /// Returns an error only when the enumeration itself fails, never for a single unreadable process.
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats>;
}
