//! Restart (spec Phase 6): starts a process from a [`Launch`] record with `CreateProcessW`:
//! the exe as the application name, the captured command line **verbatim** (so no re-quoting
//! can change an argument), the captured working directory and a Unicode environment block.
//! The new process gets Atropos's own token; callers refuse when Atropos is elevated
//! (`history::restart`), so a restart never gains rights.

use std::ffi::c_void;

use windows::Win32::Foundation::{FILETIME, HANDLE};
use windows::Win32::Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation};
use windows::Win32::System::Threading::{
    CREATE_UNICODE_ENVIRONMENT, CreateProcessW, GetCurrentProcess, GetProcessTimes,
    OpenProcessToken, PROCESS_INFORMATION, STARTUPINFOW,
};
use windows::core::{HSTRING, PWSTR};

use super::filetime_to_u64;
use super::handle::OwnedHandle;
use crate::history::Launch;
use crate::{CoreError, ProcKey, Result};

/// `NAME=value\0…\0\0` as UTF-16, in the given order (an empty list is `\0\0`).
#[must_use]
pub fn env_block(vars: &[(String, String)]) -> Vec<u16> {
    let mut out = Vec::new();
    for (k, v) in vars {
        out.extend(k.encode_utf16());
        out.push(u16::from(b'='));
        out.extend(v.encode_utf16());
        out.push(0);
    }
    if out.is_empty() {
        out.push(0);
    }
    out.push(0);
    out
}

/// Whether Atropos itself runs elevated (an administrator token).
#[must_use]
pub fn current_process_elevated() -> bool {
    let mut token = HANDLE::default();
    // SAFETY: the pseudo-handle needs no close; `token` is a valid out pointer.
    if unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &raw mut token) }.is_err() {
        return false;
    }
    let token = OwnedHandle::new(token);
    let mut elevation = TOKEN_ELEVATION::default();
    let mut len = 0u32;
    // SAFETY: `elevation` is a writable TOKEN_ELEVATION of the size passed; `len` is valid.
    let ok = unsafe {
        GetTokenInformation(
            token.raw(),
            TokenElevation,
            Some((&raw mut elevation).cast::<c_void>()),
            u32::try_from(std::mem::size_of::<TOKEN_ELEVATION>()).unwrap_or(0),
            &raw mut len,
        )
    }
    .is_ok();
    ok && elevation.TokenIsElevated != 0
}

/// Starts `l` and returns the new process's key.
///
/// # Errors
/// If `CreateProcessW` (or reading the new process's create time) fails.
pub fn launch(l: &Launch) -> Result<ProcKey> {
    let app = HSTRING::from(l.exe.as_str());
    let cwd = HSTRING::from(l.cwd.as_str());
    // CreateProcessW may write into the command-line buffer, so it is a private copy.
    let mut cmdline: Vec<u16> = l.cmdline.encode_utf16().chain(Some(0)).collect();
    let env = env_block(&l.env);
    let si = STARTUPINFOW {
        cb: u32::try_from(std::mem::size_of::<STARTUPINFOW>()).unwrap_or(0),
        ..STARTUPINFOW::default()
    };
    let mut pi = PROCESS_INFORMATION::default();
    // SAFETY: every string is NUL-terminated and outlives the call; `env` is a double-NUL
    // terminated UTF-16 block (CREATE_UNICODE_ENVIRONMENT); `si` / `pi` are valid. No handles
    // are inherited.
    unsafe {
        CreateProcessW(
            &app,
            Some(PWSTR(cmdline.as_mut_ptr())),
            None,
            None,
            false,
            CREATE_UNICODE_ENVIRONMENT,
            Some(env.as_ptr().cast::<c_void>()),
            &cwd,
            &raw const si,
            &raw mut pi,
        )
    }
    .map_err(|e| CoreError::Os {
        api: "CreateProcessW",
        code: e.code().0,
    })?;
    let process = OwnedHandle::new(pi.hProcess);
    drop(OwnedHandle::new(pi.hThread));
    let (mut created, mut exit, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: `process` is the handle CreateProcessW just returned; the out pointers are valid.
    unsafe {
        GetProcessTimes(
            process.raw(),
            &raw mut created,
            &raw mut exit,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .map_err(|e| CoreError::Os {
        api: "GetProcessTimes",
        code: e.code().0,
    })?;
    Ok(ProcKey {
        pid: pi.dwProcessId,
        create_time: filetime_to_u64(created),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_block_is_nul_separated_and_double_nul_terminated() {
        let vars = vec![
            ("PATH".to_owned(), r"C:\bin;D:\tööls".to_owned()),
            ("A".to_owned(), "x=y".to_owned()),
        ];
        let want: Vec<u16> = "PATH=C:\\bin;D:\\tööls\0A=x=y\0\0".encode_utf16().collect();
        assert_eq!(env_block(&vars), want);
        assert_eq!(env_block(&[]), vec![0, 0]);
    }

    #[test]
    fn a_test_run_is_not_elevated_or_says_so() {
        // Only checks the call works; CI may or may not run elevated.
        let _ = current_process_elevated();
    }
}
