//! Running services and their host PIDs (`EnumServicesStatusExW(SC_ENUM_PROCESS_INFO)`).
//! Needs only `SC_MANAGER_ENUMERATE_SERVICE`, which standard users have.

use std::mem::size_of;
use std::sync::Arc;

use windows::Win32::System::Services::{
    CloseServiceHandle, ENUM_SERVICE_STATUS_PROCESSW, EnumServicesStatusExW, OpenSCManagerW,
    SC_ENUM_PROCESS_INFO, SC_HANDLE, SC_MANAGER_ENUMERATE_SERVICE, SERVICE_ACTIVE, SERVICE_WIN32,
};
use windows::core::{PCWSTR, PWSTR};

use crate::ports::ServiceInfo;
use crate::{CoreError, Result};

const ERROR_MORE_DATA: i32 = 234;

/// Closes the service-manager handle on drop.
struct Scm(SC_HANDLE);

impl Drop for Scm {
    fn drop(&mut self) {
        // SAFETY: we own the handle and close it once.
        let _ = unsafe { CloseServiceHandle(self.0) };
    }
}

fn pwstr(p: PWSTR) -> Arc<str> {
    if p.is_null() {
        return Arc::from("");
    }
    // SAFETY: the service manager wrote NUL-terminated strings into our buffer.
    Arc::from(unsafe { p.to_string() }.unwrap_or_default())
}

fn os_err(e: &windows::core::Error) -> CoreError {
    CoreError::Os {
        api: "EnumServicesStatusExW",
        code: e.code().0 & 0xFFFF,
    }
}

/// All active Win32 services.
pub(crate) fn services() -> Result<Vec<ServiceInfo>> {
    // SAFETY: plain call; the handle is closed by `Scm`.
    let scm = Scm(unsafe {
        OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_ENUMERATE_SERVICE)
    }
    .map_err(|e| CoreError::Os {
        api: "OpenSCManagerW",
        code: e.code().0 & 0xFFFF,
    })?);
    let mut out = Vec::new();
    let mut resume = 0u32;
    let mut buf: Vec<u64> = vec![0; 64 * 1024 / 8];
    loop {
        let (mut needed, mut returned) = (0u32, 0u32);
        // SAFETY: `buf` is 8-aligned, so the byte view satisfies ENUM_SERVICE_STATUS_PROCESSW's
        // alignment; it lives across the call.
        let bytes =
            unsafe { std::slice::from_raw_parts_mut(buf.as_mut_ptr().cast::<u8>(), buf.len() * 8) };
        // SAFETY: all out-pointers are valid; the resume handle continues a partial listing.
        let r = unsafe {
            EnumServicesStatusExW(
                scm.0,
                SC_ENUM_PROCESS_INFO,
                SERVICE_WIN32,
                SERVICE_ACTIVE,
                Some(bytes),
                &raw mut needed,
                &raw mut returned,
                Some(&raw mut resume),
                PCWSTR::null(),
            )
        };
        let more = match &r {
            Ok(()) => false,
            Err(e) if e.code().0 & 0xFFFF == ERROR_MORE_DATA => true,
            Err(e) => return Err(os_err(e)),
        };
        let base = buf.as_ptr().cast::<ENUM_SERVICE_STATUS_PROCESSW>();
        let fits = buf.len() * 8 / size_of::<ENUM_SERVICE_STATUS_PROCESSW>();
        for i in 0..(returned as usize).min(fits) {
            // SAFETY: the first `returned` entries were written by the OS (bounded by `fits`).
            let e = unsafe { std::ptr::read(base.add(i)) };
            out.push(ServiceInfo {
                pid: e.ServiceStatusProcess.dwProcessId,
                name: pwstr(e.lpServiceName),
                display: pwstr(e.lpDisplayName),
            });
        }
        if !more {
            return Ok(out);
        }
        if returned == 0 {
            buf = vec![0; (needed as usize + 4096).div_ceil(8)];
        }
    }
}
