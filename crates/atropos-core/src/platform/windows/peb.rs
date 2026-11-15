//! Tier 2: working directory and environment of one process, read from its PEB
//! (`PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ`, on demand only).
//!
//! Offsets (phnt `ntpebteb.h` / `ntrtl.h`), stable since Windows 7:
//! - 64-bit: `PEB.ProcessParameters` @0x20; `RTL_USER_PROCESS_PARAMETERS`:
//!   `CurrentDirectory.DosPath` @0x38, `Environment` @0x80, `EnvironmentSize` @0x3F0.
//! - WOW64 (32-bit target): `PEB32.ProcessParameters` @0x10; parameters:
//!   `CurrentDirectory.DosPath` @0x24, `Environment` @0x48, `EnvironmentSize` @0x290.

use std::mem::{MaybeUninit, size_of};
use std::sync::Arc;

use windows::Wdk::System::Threading::{
    NtQueryInformationProcess, ProcessBasicInformation, ProcessWow64Information,
};
use windows::Win32::Foundation::{FILETIME, HANDLE, STILL_ACTIVE};
use windows::Win32::System::Diagnostics::Debug::ReadProcessMemory;
use windows::Win32::System::SystemInformation::IMAGE_FILE_MACHINE_UNKNOWN;
use windows::Win32::System::Threading::{
    GetExitCodeProcess, GetProcessTimes, IsWow64Process2, OpenProcess,
    PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_VM_READ,
};

use super::filetime_to_u64;
use super::handle::OwnedHandle;
use super::statics::open_error_reason;
use crate::platform::ProcEnv;
use crate::{Field, ProcKey};

/// Largest environment block we read (Windows caps a block at 32 767 chars, but be generous).
const MAX_ENV_BYTES: usize = 4 * 1024 * 1024;
/// Used when `EnvironmentSize` is 0: read this much and stop at the double NUL.
const FALLBACK_ENV_BYTES: usize = 64 * 1024;

/// Splits a UTF-16 environment block (`NAME=VALUE\0…\0\0`) into pairs. Entries that start with
/// `=` (per-drive current directories such as `=C:=C:\x`) are skipped.
pub(crate) fn parse_env_block(block: &[u16]) -> Vec<(String, String)> {
    block
        .split(|&c| c == 0)
        .take_while(|entry| !entry.is_empty())
        .filter(|entry| entry.first() != Some(&u16::from(b'=')))
        .filter_map(|entry| {
            let s = String::from_utf16_lossy(entry);
            s.split_once('=').map(|(n, v)| (n.to_owned(), v.to_owned()))
        })
        .collect()
}

/// Reads one `T` from the target at `addr`.
fn read_val<T: Copy>(h: HANDLE, addr: usize) -> Option<T> {
    let mut v = MaybeUninit::<T>::uninit();
    let mut got = 0usize;
    // SAFETY: `v` has room for exactly `size_of::<T>()` bytes; `got` is a valid out-pointer.
    unsafe {
        ReadProcessMemory(
            h,
            addr as *const _,
            v.as_mut_ptr().cast(),
            size_of::<T>(),
            Some(&raw mut got),
        )
    }
    .ok()?;
    // SAFETY: the OS wrote all `size_of::<T>()` bytes (checked), and `T: Copy` is plain data.
    (got == size_of::<T>()).then(|| unsafe { v.assume_init() })
}

/// Reads `bytes` bytes of UTF-16 from the target at `addr` (rounded down to whole units).
fn read_utf16(h: HANDLE, addr: usize, bytes: usize) -> Option<Vec<u16>> {
    let mut buf = vec![0u16; bytes / 2];
    let mut got = 0usize;
    // SAFETY: `buf` has room for `buf.len() * 2` bytes; `got` is a valid out-pointer.
    unsafe {
        ReadProcessMemory(
            h,
            addr as *const _,
            buf.as_mut_ptr().cast(),
            buf.len() * 2,
            Some(&raw mut got),
        )
    }
    .ok()?;
    buf.truncate(got / 2);
    Some(buf)
}

/// Where the cwd and environment live, per bitness.
struct Layout {
    params: usize,
    cwd_len: usize,
    cwd_buf: usize,
    env_ptr: usize,
    env_size: usize,
}

fn locate(h: HANDLE, wow64: bool) -> Option<Layout> {
    if wow64 {
        let mut peb32 = 0usize;
        // SAFETY: one usize out-buffer of the size passed.
        let st = unsafe {
            NtQueryInformationProcess(
                h,
                ProcessWow64Information,
                (&raw mut peb32).cast(),
                u32::try_from(size_of::<usize>()).ok()?,
                std::ptr::null_mut(),
            )
        };
        if st.is_err() || peb32 == 0 {
            return None;
        }
        let params = read_val::<u32>(h, peb32 + 0x10)? as usize;
        Some(Layout {
            params,
            cwd_len: usize::from(read_val::<u16>(h, params + 0x24)?),
            cwd_buf: read_val::<u32>(h, params + 0x28)? as usize,
            env_ptr: read_val::<u32>(h, params + 0x48)? as usize,
            env_size: read_val::<u32>(h, params + 0x290)? as usize,
        })
    } else {
        // PROCESS_BASIC_INFORMATION is six pointer-sized fields; PebBaseAddress is the second.
        let mut pbi = [0usize; 6];
        // SAFETY: `pbi` is exactly the size passed.
        let st = unsafe {
            NtQueryInformationProcess(
                h,
                ProcessBasicInformation,
                pbi.as_mut_ptr().cast(),
                u32::try_from(size_of::<[usize; 6]>()).ok()?,
                std::ptr::null_mut(),
            )
        };
        if st.is_err() || pbi[1] == 0 {
            return None;
        }
        let params = read_val::<usize>(h, pbi[1] + 0x20)?;
        Some(Layout {
            params,
            cwd_len: usize::from(read_val::<u16>(h, params + 0x38)?),
            cwd_buf: read_val::<usize>(h, params + 0x40)?,
            env_ptr: read_val::<usize>(h, params + 0x80)?,
            env_size: read_val::<usize>(h, params + 0x3F0)?,
        })
    }
}

fn read_env(h: HANDLE, wow64: bool, with_env: bool) -> Option<ProcEnv> {
    let l = locate(h, wow64)?;
    if l.params == 0 {
        return None;
    }
    let cwd = read_utf16(h, l.cwd_buf, l.cwd_len)?;
    if !with_env {
        return Some(ProcEnv {
            cwd: Arc::from(String::from_utf16_lossy(&cwd)),
            vars: Vec::new(),
        });
    }
    let size = match l.env_size {
        0 => FALLBACK_ENV_BYTES,
        n => n.min(MAX_ENV_BYTES),
    };
    let block = read_utf16(h, l.env_ptr, size)?;
    Some(ProcEnv {
        cwd: Arc::from(String::from_utf16_lossy(&cwd)),
        vars: parse_env_block(&block),
    })
}

fn has_exited(h: HANDLE) -> bool {
    let mut code = 0u32;
    // SAFETY: valid out-pointer.
    let ok = unsafe { GetExitCodeProcess(h, &raw mut code) }.is_ok();
    #[allow(clippy::cast_sign_loss)] // STILL_ACTIVE is 259
    let still = STILL_ACTIVE.0 as u32;
    ok && code != still
}

/// Reads the cwd and (with `with_env`) the environment of `key`.
pub(crate) fn read(key: ProcKey, with_env: bool) -> Field<ProcEnv> {
    let access = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_VM_READ;
    // SAFETY: plain call; the handle is owned by `OwnedHandle`.
    let h = match unsafe { OpenProcess(access, false, key.pid) } {
        Ok(h) => OwnedHandle::new(h),
        Err(e) => {
            let code = u32::try_from(e.code().0 & 0xFFFF).unwrap_or(0);
            return open_error_reason(code).map(|()| ProcEnv::default());
        }
    };
    let (mut created, mut exited, mut kernel, mut user) = (
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
        FILETIME::default(),
    );
    // SAFETY: four valid FILETIME out-pointers.
    let times = unsafe {
        GetProcessTimes(
            h.raw(),
            &raw mut created,
            &raw mut exited,
            &raw mut kernel,
            &raw mut user,
        )
    };
    if times.is_err() {
        return Field::Restricted;
    }
    if filetime_to_u64(created) != key.create_time {
        return Field::Gone;
    }
    let mut machine = IMAGE_FILE_MACHINE_UNKNOWN;
    // SAFETY: valid out-pointer; the native machine is optional.
    let wow64 = unsafe { IsWow64Process2(h.raw(), &raw mut machine, None) }.is_ok()
        && machine != IMAGE_FILE_MACHINE_UNKNOWN;
    match read_env(h.raw(), wow64, with_env) {
        Some(env) => Field::Ok(env),
        None if has_exited(h.raw()) => Field::Gone,
        None => Field::Restricted,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block(s: &str) -> Vec<u16> {
        s.encode_utf16().collect()
    }

    #[test]
    fn parse_env_block_skips_drive_vars_and_stops_at_double_nul() {
        let b = block("=C:=C:\\x\0PATH=C:\\a;C:\\b\0NAME=Jos\u{e9}\0EMPTY=\0WEIRD\0\0GARBAGE=1\0");
        assert_eq!(
            parse_env_block(&b),
            vec![
                ("PATH".to_owned(), r"C:\a;C:\b".to_owned()),
                ("NAME".to_owned(), "José".to_owned()),
                ("EMPTY".to_owned(), String::new()),
            ]
        );
    }

    #[test]
    fn value_may_contain_equals() {
        let b = block("OPTS=a=b=c\0\0");
        assert_eq!(
            parse_env_block(&b),
            vec![("OPTS".to_owned(), "a=b=c".to_owned())]
        );
    }

    #[test]
    fn unterminated_block_is_parsed_to_its_end() {
        assert_eq!(
            parse_env_block(&block("A=1\0B=2")),
            vec![("A".into(), "1".into()), ("B".into(), "2".into())]
        );
    }
}
