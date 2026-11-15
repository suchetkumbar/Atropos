//! Pinning on Windows: `OpenProcess` with the minimum rights, then a create-time re-check on
//! the handle. Exit waits use `WaitForMultipleObjects` in chunks of 64.

use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HANDLE, WAIT_OBJECT_0, WAIT_TIMEOUT};
use windows::Win32::System::Threading::{
    OpenProcess, PROCESS_ACCESS_RIGHTS, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_INFORMATION,
    PROCESS_SET_QUOTA, PROCESS_SUSPEND_RESUME, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
    TerminateProcess, WaitForMultipleObjects, WaitForSingleObject,
};

use super::handle::OwnedHandle;
use super::statics::{create_time, win32_code};
use crate::pin::{Access, PinError, PinHandle, Pinned};
use crate::{CoreError, ProcKey};

const ERROR_ACCESS_DENIED: u32 = 5;
const ERROR_INVALID_PARAMETER: u32 = 87;
/// `MAXIMUM_WAIT_OBJECTS`.
const CHUNK: usize = 64;
/// Wait slice per chunk when there is more than one chunk.
const SLICE: Duration = Duration::from_millis(20);

/// The rights requested for `access`. Query + synchronize always; nothing destructive unless
/// asked for.
pub(crate) fn mask(access: Access) -> PROCESS_ACCESS_RIGHTS {
    let mut m = PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE;
    if access.terminate {
        // SET_QUOTA is required to put the process into a Job Object (subtree kill).
        m |= PROCESS_TERMINATE | PROCESS_SET_QUOTA;
    }
    if access.suspend {
        m |= PROCESS_SUSPEND_RESUME;
    }
    if access.tune {
        m |= PROCESS_SET_INFORMATION;
    }
    m
}

pub(crate) fn pin(key: ProcKey, access: Access) -> Result<Pinned, PinError> {
    // SAFETY: plain call; ownership of the handle moves into `OwnedHandle`.
    let h = match unsafe { OpenProcess(mask(access), false, key.pid) } {
        Ok(h) => OwnedHandle::new(h),
        Err(e) => {
            return Err(match win32_code(&e) {
                ERROR_INVALID_PARAMETER => PinError::Gone,
                ERROR_ACCESS_DENIED => PinError::Denied,
                code => PinError::Os(CoreError::Os {
                    api: "OpenProcess",
                    code: i32::try_from(code).unwrap_or(i32::MAX),
                }),
            });
        }
    };
    match create_time(h.raw()) {
        // Exited but still referenced (e.g. by its console host): nothing to act on.
        Some(t) if t == key.create_time && signaled(h.raw()) => Err(PinError::Gone),
        Some(t) if t == key.create_time => Ok(Pinned::new(key, access, PinHandle::Windows(h))),
        Some(_) => Err(PinError::Reused),
        None => Err(PinError::Denied),
    }
}

/// `TerminateProcess(handle, 1)` through the pin.
pub(crate) fn terminate(p: &Pinned) -> crate::Result<()> {
    let Some(h) = raw(p) else {
        return Err(CoreError::Unsupported);
    };
    // SAFETY: a live process handle; `pin` requested PROCESS_TERMINATE for `Access::KILL`.
    unsafe { TerminateProcess(h, 1) }.map_err(|e| CoreError::Os {
        api: "TerminateProcess",
        code: i32::try_from(win32_code(&e)).unwrap_or(i32::MAX),
    })
}

/// Full image path of a pinned process.
pub(crate) fn exe_path(p: &Pinned) -> Option<String> {
    let h = raw(p)?;
    super::statics::exe_path(h).ok().map(ToString::to_string)
}

pub(crate) fn raw(p: &Pinned) -> Option<HANDLE> {
    match &p.handle {
        PinHandle::Windows(h) => Some(h.raw()),
        PinHandle::Fake => None,
    }
}

fn signaled(h: HANDLE) -> bool {
    // SAFETY: `h` is a live process handle with SYNCHRONIZE (see `mask`).
    let r = unsafe { WaitForSingleObject(h, 0) };
    r == WAIT_OBJECT_0
}

pub(crate) fn wait_exits(pins: &[&Pinned], timeout: Duration) -> Vec<bool> {
    let handles: Vec<HANDLE> = pins.iter().filter_map(|p| raw(p)).collect();
    let check = || {
        pins.iter()
            .map(|p| raw(p).is_some_and(signaled))
            .collect::<Vec<_>>()
    };
    let now = check();
    if now.iter().any(|e| *e) || handles.is_empty() || timeout.is_zero() {
        return now;
    }
    let deadline = Instant::now() + timeout;
    let chunks: Vec<&[HANDLE]> = handles.chunks(CHUNK).collect();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return check();
        }
        let slice = if chunks.len() == 1 {
            left
        } else {
            left.min(SLICE)
        };
        let ms = u32::try_from(slice.as_millis()).unwrap_or(u32::MAX);
        for chunk in &chunks {
            // SAFETY: up to 64 live process handles with SYNCHRONIZE.
            let r = unsafe { WaitForMultipleObjects(chunk, false, ms) };
            if r != WAIT_TIMEOUT {
                return check();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dry_run_access_never_includes_terminate_or_suspend() {
        let m = mask(Access::QUERY);
        assert_eq!(m & PROCESS_TERMINATE, PROCESS_ACCESS_RIGHTS(0));
        assert_eq!(m & PROCESS_SUSPEND_RESUME, PROCESS_ACCESS_RIGHTS(0));
        assert_ne!(m & PROCESS_SYNCHRONIZE, PROCESS_ACCESS_RIGHTS(0));
        assert_ne!(
            m & PROCESS_QUERY_LIMITED_INFORMATION,
            PROCESS_ACCESS_RIGHTS(0)
        );
    }

    #[test]
    fn kill_access_has_terminate_and_suspend() {
        let m = mask(Access::KILL);
        assert_ne!(m & PROCESS_TERMINATE, PROCESS_ACCESS_RIGHTS(0));
        assert_ne!(m & PROCESS_SUSPEND_RESUME, PROCESS_ACCESS_RIGHTS(0));
    }
}
