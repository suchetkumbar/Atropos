//! Which processes have files open, via Restart Manager (spec §4.4, Phase 6). Read-only: the
//! session only registers files and lists their users; nothing is shut down or restarted.
//!
//! `RM_PROCESS_INFO.Process` carries the PID **and** the start time, so each result maps
//! straight to a [`ProcKey`] (no PID-reuse guess). The session is always ended (RAII); a user may
//! hold at most 64 sessions at once. See `docs/notes/windows-apis.md`.

use std::os::windows::ffi::OsStrExt as _;
use std::path::PathBuf;

use windows::Win32::Foundation::{ERROR_MORE_DATA, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::RestartManager::{
    CCH_RM_SESSION_KEY, RM_PROCESS_INFO, RmEndSession, RmGetList, RmRegisterResources,
    RmStartSession,
};
use windows::core::{PCWSTR, PWSTR};

use super::filetime_to_u64;
use crate::{CoreError, ProcKey, Result};

/// Files per `RmRegisterResources` call.
const CHUNK: usize = 256;
/// `RmGetList` retries while the list keeps growing between calls.
const MAX_ATTEMPTS: usize = 5;

#[allow(clippy::cast_possible_wrap)] // Win32 error codes as reported
fn os(api: &'static str, e: WIN32_ERROR) -> CoreError {
    CoreError::Os {
        api,
        code: e.0 as i32,
    }
}

/// A Restart Manager session, ended on drop.
struct Session(u32);

impl Session {
    fn start() -> Result<Self> {
        let mut handle = 0u32;
        let mut key = [0u16; CCH_RM_SESSION_KEY as usize + 1];
        // SAFETY: `handle` is a valid out pointer; `key` has the CCH_RM_SESSION_KEY + 1 WCHARs
        // the API writes the session key into.
        let r = unsafe { RmStartSession(&raw mut handle, None, PWSTR(key.as_mut_ptr())) };
        if r != ERROR_SUCCESS {
            return Err(os("RmStartSession", r));
        }
        Ok(Self(handle))
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // SAFETY: `self.0` is a session this value started and has not ended.
        let _ = unsafe { RmEndSession(self.0) };
    }
}

/// The processes that have any of `files` open, sorted by PID, without duplicates.
///
/// # Errors
/// If Restart Manager refuses the session, a file name, or the list.
pub fn lockers(files: &[PathBuf]) -> Result<Vec<ProcKey>> {
    if files.is_empty() {
        return Ok(Vec::new());
    }
    let session = Session::start()?;
    let wide: Vec<Vec<u16>> = files
        .iter()
        .map(|p| p.as_os_str().encode_wide().chain(Some(0)).collect())
        .collect();
    for chunk in wide.chunks(CHUNK) {
        let names: Vec<PCWSTR> = chunk.iter().map(|w| PCWSTR(w.as_ptr())).collect();
        // SAFETY: every PCWSTR points into a NUL-terminated buffer in `wide`, alive for the call.
        let r = unsafe { RmRegisterResources(session.0, Some(&names), None, None) };
        if r != ERROR_SUCCESS {
            return Err(os("RmRegisterResources", r));
        }
    }
    let mut buf: Vec<RM_PROCESS_INFO> = Vec::new();
    for _ in 0..MAX_ATTEMPTS {
        let (mut needed, mut reasons) = (0u32, 0u32);
        let mut n = u32::try_from(buf.len()).unwrap_or(u32::MAX);
        let out = (!buf.is_empty()).then_some(buf.as_mut_ptr());
        // SAFETY: `out` is null with `n == 0`, or points to `n` writable RM_PROCESS_INFO; the
        // other arguments are valid out pointers.
        let r = unsafe {
            RmGetList(
                session.0,
                &raw mut needed,
                &raw mut n,
                out,
                &raw mut reasons,
            )
        };
        if r == ERROR_SUCCESS {
            buf.truncate(n as usize);
            let mut keys: Vec<ProcKey> = buf
                .iter()
                .map(|i| ProcKey {
                    pid: i.Process.dwProcessId,
                    create_time: filetime_to_u64(i.Process.ProcessStartTime),
                })
                .collect();
            keys.sort_by_key(|k| (k.pid, k.create_time));
            keys.dedup();
            return Ok(keys);
        }
        if r != ERROR_MORE_DATA {
            return Err(os("RmGetList", r));
        }
        // A little headroom: the list can grow between the two calls.
        buf.resize(needed as usize + 4, RM_PROCESS_INFO::default());
    }
    Err(os("RmGetList", ERROR_MORE_DATA))
}
