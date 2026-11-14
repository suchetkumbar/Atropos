//! Tier-1 statics on Windows. Every read goes through one `PROCESS_QUERY_LIMITED_INFORMATION`
//! handle whose create time is re-checked first, so a reused PID can never leak another
//! process's facts.

use std::collections::HashMap;
use std::mem::size_of;
use std::sync::Arc;

use windows::Wdk::System::Threading::{
    NtQueryInformationProcess, PROCESSINFOCLASS, ProcessCommandLineInformation,
    ProcessProtectionInformation,
};
use windows::Win32::Foundation::{FILETIME, HANDLE, STILL_ACTIVE, UNICODE_STRING};
use windows::Win32::Security::{
    GetLengthSid, GetTokenInformation, LookupAccountSidW, SID_NAME_USE, TOKEN_QUERY, TOKEN_USER,
    TokenUser,
};
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, IsWow64Process2, OpenProcess, OpenProcessToken,
    PROCESS_NAME_WIN32, PROCESS_QUERY_LIMITED_INFORMATION, QueryFullProcessImageNameW,
};
use windows::core::{PCWSTR, PWSTR};

use super::filetime_to_u64;
use super::handle::OwnedHandle;
use crate::statics::{Protection, Statics};
use crate::{Field, ProcKey};

/// Win32 error from `OpenProcess` when no process has the PID.
const ERROR_INVALID_PARAMETER: u32 = 87;
/// NTSTATUS values that mean "retry with the returned size".
#[allow(clippy::cast_possible_wrap)] // NTSTATUS bit patterns
const RETRY_STATUSES: [i32; 2] = [0xC000_0004_u32 as i32, 0xC000_0023_u32 as i32];
/// Upper bound for a command line (the documented limit is 32 767 UTF-16 units).
const MAX_CMDLINE_BYTES: usize = 64 * 1024 + 64;

/// `OpenProcess` failure → why the fields are unavailable. `ERROR_INVALID_PARAMETER` means no
/// process has that PID any more.
pub(crate) fn open_error_reason(code: u32) -> Field<()> {
    match code {
        ERROR_INVALID_PARAMETER => Field::Gone,
        // ERROR_ACCESS_DENIED and anything unexpected: we may not look.
        _ => Field::Restricted,
    }
}

/// Account names by SID bytes (lookups can be slow; SIDs repeat across processes).
pub(crate) type SidCache = HashMap<Vec<u8>, Field<Arc<str>>>;

pub(crate) fn win32_code(e: &windows::core::Error) -> u32 {
    u32::try_from(e.code().0 & 0xFFFF).unwrap_or(0)
}

/// Reads the statics of `key`.
pub(crate) fn read(key: ProcKey, sids: &mut SidCache) -> Statics {
    if key.pid == 0 {
        return Statics::unavailable(&Field::Unsupported);
    }
    // SAFETY: plain call; the returned handle is owned by `OwnedHandle` below.
    let h = match unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, key.pid) } {
        Ok(h) => OwnedHandle::new(h),
        Err(e) => return Statics::unavailable(&open_error_reason(win32_code(&e))),
    };
    match create_time(h.raw()) {
        Some(t) if t == key.create_time => {}
        Some(_) => return Statics::unavailable(&Field::Gone),
        None => return Statics::unavailable(&Field::Restricted),
    }
    let s = Statics {
        exe_path: exe_path(h.raw()),
        cmdline: cmdline(h.raw()),
        user: user(h.raw(), sids),
        wow64: wow64(h.raw()),
        protection: protection(h.raw()),
    };
    if has_exited(h.raw()) {
        // It exited while we read: whatever failed did so because it is gone.
        return Statics {
            exe_path: gone_if_failed(s.exe_path),
            cmdline: gone_if_failed(s.cmdline),
            user: gone_if_failed(s.user),
            wow64: gone_if_failed(s.wow64),
            protection: gone_if_failed(s.protection),
        };
    }
    s
}

fn gone_if_failed<T>(f: Field<T>) -> Field<T> {
    match f {
        Field::Restricted => Field::Gone,
        other => other,
    }
}

pub(crate) fn create_time(h: HANDLE) -> Option<u64> {
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: four valid out-pointers to FILETIMEs on our stack.
    unsafe {
        GetProcessTimes(
            h,
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    }
    .ok()?;
    Some(filetime_to_u64(created))
}

fn has_exited(h: HANDLE) -> bool {
    let mut code = 0u32;
    // SAFETY: valid out-pointer; LIMITED access suffices for GetExitCodeProcess.
    let ok = unsafe { GetExitCodeProcess(h, &raw mut code) }.is_ok();
    #[allow(clippy::cast_sign_loss)] // STILL_ACTIVE is 259
    let still = STILL_ACTIVE.0 as u32;
    ok && code != still
}

pub(crate) fn exe_path(h: HANDLE) -> Field<Arc<str>> {
    let mut buf = vec![0u16; 32_768];
    let mut len = u32::try_from(buf.len()).unwrap_or(0);
    // SAFETY: `buf` holds `len` UTF-16 units; `len` is a valid in/out pointer.
    match unsafe {
        QueryFullProcessImageNameW(h, PROCESS_NAME_WIN32, PWSTR(buf.as_mut_ptr()), &raw mut len)
    } {
        Ok(()) => Field::Ok(Arc::from(String::from_utf16_lossy(&buf[..len as usize]))),
        Err(_) => Field::Restricted,
    }
}

/// `NtQueryInformationProcess` for a class that returns a `UNICODE_STRING` header followed by
/// its characters (`ProcessCommandLineInformation`).
fn query_unicode(h: HANDLE, class: PROCESSINFOCLASS) -> Field<Arc<str>> {
    let mut buf = vec![0u64; 512];
    loop {
        let cap = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        let mut ret = 0u32;
        // SAFETY: `buf` is 8-aligned and `cap` bytes long; `ret` is a valid out-pointer.
        let st = unsafe {
            NtQueryInformationProcess(h, class, buf.as_mut_ptr().cast(), cap, &raw mut ret)
        };
        if RETRY_STATUSES.contains(&st.0)
            && (ret as usize) > buf.len() * 8
            && (ret as usize) <= MAX_CMDLINE_BYTES
        {
            buf.resize((ret as usize).div_ceil(8), 0);
            continue;
        }
        if st.is_err() {
            return Field::Restricted;
        }
        let bytes = (buf.len() * 8).min(ret as usize);
        if bytes < size_of::<UNICODE_STRING>() {
            return Field::Restricted;
        }
        // SAFETY: `buf` starts with an 8-aligned UNICODE_STRING the OS wrote (size checked).
        let us: UNICODE_STRING = unsafe { std::ptr::read(buf.as_ptr().cast()) };
        let lo = buf.as_ptr() as usize;
        let (p, n) = (us.Buffer.0 as usize, usize::from(us.Length));
        if n == 0 {
            return Field::Ok(Arc::from(""));
        }
        if p < lo || p + n > lo + bytes || p % 2 != 0 {
            return Field::Restricted;
        }
        // SAFETY: the characters lie inside `buf` (bounds and alignment checked above).
        let units = unsafe { std::slice::from_raw_parts(p as *const u16, n / 2) };
        return Field::Ok(Arc::from(String::from_utf16_lossy(units)));
    }
}

fn cmdline(h: HANDLE) -> Field<Arc<str>> {
    query_unicode(h, ProcessCommandLineInformation)
}

fn protection(h: HANDLE) -> Field<Protection> {
    let mut b = 0u8;
    let mut ret = 0u32;
    // SAFETY: one writable byte (PS_PROTECTION) and a valid out-pointer.
    let st = unsafe {
        NtQueryInformationProcess(
            h,
            ProcessProtectionInformation,
            (&raw mut b).cast(),
            1,
            &raw mut ret,
        )
    };
    if st.is_err() {
        return Field::Restricted;
    }
    // PS_PROTECTION.Type is the low 3 bits: 0 none, 1 light, 2 full.
    Field::Ok(match b & 0b111 {
        0 => Protection::None,
        1 => Protection::Light,
        _ => Protection::Full,
    })
}

fn wow64(h: HANDLE) -> Field<bool> {
    let mut machine = IMAGE_FILE_MACHINE_UNKNOWN;
    // SAFETY: valid out-pointer; the native-machine output is optional.
    match unsafe { IsWow64Process2(h, &raw mut machine, None) } {
        Ok(()) => Field::Ok(machine != IMAGE_FILE_MACHINE_UNKNOWN),
        Err(_) => Field::Restricted,
    }
}

fn user(h: HANDLE, sids: &mut SidCache) -> Field<Arc<str>> {
    let mut tok = HANDLE::default();
    // SAFETY: valid out-pointer; ownership moves into `OwnedHandle`.
    if unsafe { OpenProcessToken(h, TOKEN_QUERY, &raw mut tok) }.is_err() {
        return Field::Restricted;
    }
    let tok = OwnedHandle::new(tok);
    let mut buf = vec![0u64; 16];
    let mut ret = 0u32;
    for _ in 0..2 {
        let cap = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `buf` is 8-aligned and `cap` bytes; `ret` is a valid out-pointer.
        let r = unsafe {
            GetTokenInformation(
                tok.raw(),
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                cap,
                &raw mut ret,
            )
        };
        if r.is_ok() {
            break;
        }
        if ret as usize <= buf.len() * 8 {
            return Field::Restricted;
        }
        buf.resize((ret as usize).div_ceil(8), 0);
    }
    if (ret as usize) < size_of::<TOKEN_USER>() {
        return Field::Restricted;
    }
    // SAFETY: GetTokenInformation succeeded and wrote a TOKEN_USER at the 8-aligned start.
    let tu: TOKEN_USER = unsafe { std::ptr::read(buf.as_ptr().cast()) };
    let psid = tu.User.Sid;
    // SAFETY: `psid` points at a valid SID inside `buf` (written by the OS).
    let sid_len = unsafe { GetLengthSid(psid) } as usize;
    // SAFETY: the SID occupies `sid_len` bytes at `psid`, inside `buf`, which outlives this slice.
    let sid = unsafe { std::slice::from_raw_parts(psid.0 as *const u8, sid_len) }.to_vec();
    if let Some(name) = sids.get(&sid) {
        return name.clone();
    }
    let mut name = [0u16; 256];
    let mut dom = [0u16; 256];
    let (mut n, mut d) = (256u32, 256u32);
    let mut use_ = SID_NAME_USE::default();
    // SAFETY: both buffers hold the counts passed; all out-pointers are valid.
    let r = unsafe {
        LookupAccountSidW(
            PCWSTR::null(),
            psid,
            Some(PWSTR(name.as_mut_ptr())),
            &raw mut n,
            Some(PWSTR(dom.as_mut_ptr())),
            &raw mut d,
            &raw mut use_,
        )
    };
    let field = match r {
        Ok(()) => Field::Ok(Arc::from(String::from_utf16_lossy(
            &name[..(n as usize).min(256)],
        ))),
        Err(_) => Field::Unsupported,
    };
    sids.insert(sid, field.clone());
    field
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_denied_is_restricted() {
        assert_eq!(open_error_reason(5), Field::Restricted);
    }

    #[test]
    fn invalid_parameter_is_gone() {
        assert_eq!(open_error_reason(87), Field::Gone);
    }

    #[test]
    fn other_errors_are_restricted() {
        assert_eq!(open_error_reason(31), Field::Restricted);
    }
}
