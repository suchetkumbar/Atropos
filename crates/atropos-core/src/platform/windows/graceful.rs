//! Graceful stop strategies on Windows, tried in this order: stop the services the process
//! hosts; `WM_CLOSE` to its visible unowned top-level windows; console Ctrl+C via the helper.

use std::io::Read as _;
use std::os::windows::process::CommandExt as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use windows::Win32::Foundation::{HWND, LPARAM, WPARAM};
use windows::Win32::System::Services::{
    ControlService, OpenSCManagerW, OpenServiceW, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP,
    SERVICE_STATUS, SERVICE_STOP,
};
use windows::Win32::UI::WindowsAndMessaging::{
    EnumWindows, GW_OWNER, GetWindow, GetWindowThreadProcessId, IsWindowVisible, PostMessageW,
    WM_CLOSE,
};
use windows::core::{BOOL, HSTRING, PCWSTR};

use crate::engine::kill::Strategy;
use crate::pin::Pinned;
use crate::{CoreError, Result};

/// `DETACHED_PROCESS`: the helper starts with no console (it attaches to the target's).
const DETACHED_PROCESS: u32 = 0x0000_0008;
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
/// How long the helper may take.
const HELPER_TIMEOUT: Duration = Duration::from_secs(5);
/// `IMAGE_SUBSYSTEM_WINDOWS_CUI`.
const SUBSYSTEM_CONSOLE: u16 = 3;

/// The PE optional-header `Subsystem` of an exe image (first 4 KiB is enough).
pub(crate) fn pe_subsystem(head: &[u8]) -> Option<u16> {
    let u32_at = |o: usize| {
        head.get(o..o + 4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    };
    let u16_at = |o: usize| head.get(o..o + 2).map(|b| u16::from_le_bytes([b[0], b[1]]));
    if head.get(0..2)? != b"MZ" {
        return None;
    }
    let pe = u32_at(0x3C)? as usize;
    if head.get(pe..pe + 4)? != b"PE\0\0" {
        return None;
    }
    // COFF header is 20 bytes after the signature; Subsystem is at offset 68 of the optional
    // header for both PE32 and PE32+.
    u16_at(pe + 4 + 20 + 68)
}

fn is_console_app(exe: &str) -> bool {
    let mut head = vec![0u8; 4096];
    let n = std::fs::File::open(exe)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    pe_subsystem(&head[..n]) == Some(SUBSYSTEM_CONSOLE)
}

struct Search {
    pid: u32,
    found: Vec<HWND>,
}

unsafe extern "system" fn collect(hwnd: HWND, lparam: LPARAM) -> BOOL {
    // SAFETY: `lparam` is the `&mut Search` passed to EnumWindows, alive for the whole call.
    let s = unsafe { &mut *(lparam.0 as *mut Search) };
    let mut pid = 0u32;
    // SAFETY: `hwnd` comes from EnumWindows; `pid` is a valid out-pointer.
    unsafe {
        GetWindowThreadProcessId(hwnd, Some(&raw mut pid));
        if pid == s.pid && IsWindowVisible(hwnd).as_bool() && GetWindow(hwnd, GW_OWNER).is_err() {
            s.found.push(hwnd);
        }
    }
    BOOL(1)
}

/// Visible, unowned top-level windows of `pid`.
fn windows_of(pid: u32) -> Vec<HWND> {
    let mut s = Search {
        pid,
        found: Vec::new(),
    };
    // SAFETY: `collect` only uses `lparam` as `&mut Search`, which outlives the call.
    let _ = unsafe { EnumWindows(Some(collect), LPARAM(&raw mut s as isize)) };
    s.found
}

fn hosted_services(pid: u32) -> Vec<String> {
    super::services::services()
        .map(|all| {
            all.into_iter()
                .filter(|s| s.pid == pid)
                .map(|s| s.name.to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// Picks the graceful strategy for `p`.
pub(crate) fn strategy(p: &Pinned, helper: Option<&Path>) -> Strategy {
    let pid = p.key().pid;
    let services = hosted_services(pid);
    if !services.is_empty() {
        return Strategy::Service(services);
    }
    if !windows_of(pid).is_empty() {
        return Strategy::Window;
    }
    if helper.is_some() {
        if let Some(exe) = super::pin::exe_path(p) {
            if is_console_app(&exe) {
                return Strategy::ConsoleCtrlC;
            }
        }
    }
    Strategy::None
}

fn stop_service(name: &str) -> Result<()> {
    let err = |api: &'static str, e: &windows::core::Error| CoreError::Os {
        api,
        code: e.code().0 & 0xFFFF,
    };
    // SAFETY: plain calls; both handles are closed below.
    unsafe {
        let scm = OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT)
            .map_err(|e| err("OpenSCManagerW", &e))?;
        let svc = OpenServiceW(scm, &HSTRING::from(name), SERVICE_STOP);
        let result = match svc {
            Ok(h) => {
                let mut status = SERVICE_STATUS::default();
                let r = ControlService(h, SERVICE_CONTROL_STOP, &raw mut status)
                    .map_err(|e| err("ControlService", &e));
                let _ = windows::Win32::System::Services::CloseServiceHandle(h);
                r
            }
            Err(e) => Err(err("OpenServiceW", &e)),
        };
        let _ = windows::Win32::System::Services::CloseServiceHandle(scm);
        result
    }
}

fn run_helper(helper: &Path, p: &Pinned) -> Result<()> {
    let key = p.key();
    let mut child = Command::new(helper)
        .args([
            "__ctrl-c",
            &key.pid.to_string(),
            &key.create_time.to_string(),
        ])
        .creation_flags(DETACHED_PROCESS | CREATE_NO_WINDOW)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|_| CoreError::Unsupported)?;
    let start = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => return Ok(()),
            Ok(Some(_)) | Err(_) => return Err(CoreError::Unsupported),
            Ok(None) if start.elapsed() > HELPER_TIMEOUT => {
                // Our own helper process, through its own handle.
                let _ = child.kill();
                return Err(CoreError::Unsupported);
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

/// Sends the graceful request.
pub(crate) fn graceful(p: &Pinned, how: &Strategy, helper: Option<&Path>) -> Result<()> {
    match how {
        Strategy::Service(names) => names.iter().try_for_each(|n| stop_service(n)),
        Strategy::Window => {
            let wins = windows_of(p.key().pid);
            if wins.is_empty() {
                return Err(CoreError::Unsupported);
            }
            for w in wins {
                // SAFETY: posting a message to a window handle; no pointers.
                let _ = unsafe { PostMessageW(Some(w), WM_CLOSE, WPARAM(0), LPARAM(0)) };
            }
            Ok(())
        }
        Strategy::ConsoleCtrlC => helper.map_or(Err(CoreError::Unsupported), |h| run_helper(h, p)),
        Strategy::None => Err(CoreError::Unsupported),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pe_subsystem_reads_our_own_test_binary() {
        // Test binaries are console apps.
        let exe = std::env::current_exe().unwrap();
        assert!(is_console_app(&exe.display().to_string()));
    }

    #[test]
    fn pe_subsystem_rejects_garbage() {
        assert_eq!(pe_subsystem(b"not a pe"), None);
        assert_eq!(pe_subsystem(&[]), None);
        let mut fake = vec![0u8; 512];
        fake[0..2].copy_from_slice(b"MZ");
        fake[0x3C] = 0xFF; // e_lfanew beyond the buffer
        assert_eq!(pe_subsystem(&fake), None);
    }
}
