//! Process identity of the current process.

use windows::Win32::Foundation::FILETIME;
use windows::Win32::System::SystemInformation::GetSystemTimeAsFileTime;
use windows::Win32::System::Threading::{GetCurrentProcess, GetCurrentProcessId, GetProcessTimes};

use super::filetime_to_u64;
use crate::{CoreError, ProcKey, Result};

/// The [`ProcKey`] of the calling process.
///
/// # Errors
/// [`CoreError::Os`] if `GetProcessTimes` fails (not expected for the own process).
pub fn current_process_key() -> Result<ProcKey> {
    let mut creation = FILETIME::default();
    let mut exit = FILETIME::default();
    let mut kernel = FILETIME::default();
    let mut user = FILETIME::default();
    // SAFETY: GetCurrentProcessId/GetCurrentProcess have no preconditions; the pseudo-handle
    // needs no close. All four out-pointers are valid, writable stack locals for the call.
    let pid = unsafe {
        GetProcessTimes(
            GetCurrentProcess(),
            &raw mut creation,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
        .map_err(|e| CoreError::Os {
            api: "GetProcessTimes",
            code: e.code().0,
        })?;
        GetCurrentProcessId()
    };
    Ok(ProcKey {
        pid,
        create_time: filetime_to_u64(creation),
    })
}

/// Current system time as 100 ns ticks since 1601-01-01 UTC (same epoch as [`ProcKey::create_time`]).
#[must_use]
pub fn now_filetime() -> u64 {
    // SAFETY: no preconditions; returns the value by copy.
    filetime_to_u64(unsafe { GetSystemTimeAsFileTime() })
}
