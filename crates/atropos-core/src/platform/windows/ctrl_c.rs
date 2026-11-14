//! Console Ctrl+C delivery. Runs in a short-lived helper process (never the GUI): it detaches
//! from its own console, attaches to the target's, ignores Ctrl+C itself, and raises the event.
//! Every process on that console receives it (the shell usually ignores it).

use std::process::ExitCode;

use std::io::Write as _;
use std::os::windows::process::CommandExt as _;

use windows::Win32::System::Console::{
    AttachConsole, CTRL_C_EVENT, FreeConsole, GenerateConsoleCtrlEvent, GetConsoleProcessList,
    GetStdHandle, STD_OUTPUT_HANDLE, SetConsoleCtrlHandler, SetStdHandle,
};

use super::WindowsPlatform;
use crate::pin::Access;
use crate::platform::Platform;
use crate::{CoreError, ProcKey, Result};

fn os(api: &'static str, e: &windows::core::Error) -> CoreError {
    CoreError::Os {
        api,
        code: e.code().0 & 0xFFFF,
    }
}

/// Sends Ctrl+C to the console of `key`, after re-checking its identity.
///
/// # Errors
/// If `key` no longer matches a live process, or the console calls fail.
pub fn send_ctrl_c(key: ProcKey) -> Result<()> {
    // Identity first: a reused PID must never receive our Ctrl+C. The pin is held until the
    // event has been raised.
    let _pin = WindowsPlatform::new()
        .pin(key, Access::QUERY)
        .map_err(|_| CoreError::Unsupported)?;
    // SAFETY: console calls on our own process state; no pointers.
    unsafe {
        let _ = FreeConsole();
        AttachConsole(key.pid).map_err(|e| os("AttachConsole", &e))?;
        // Ignore the Ctrl+C we are about to raise in this helper.
        SetConsoleCtrlHandler(None, true).map_err(|e| os("SetConsoleCtrlHandler", &e))?;
        let sent = GenerateConsoleCtrlEvent(CTRL_C_EVENT, 0)
            .map_err(|e| os("GenerateConsoleCtrlEvent", &e));
        let _ = FreeConsole();
        sent
    }
}

/// PIDs on the console of `key`, via the helper (`atr.exe __console-list <pid> <create_time>`
/// prints them one per line).
///
/// # Errors
/// Without a helper, or if the helper fails.
pub(crate) fn console_processes(
    key: ProcKey,
    helper: Option<&std::path::Path>,
) -> Result<Vec<u32>> {
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    let helper = helper.ok_or(CoreError::Unsupported)?;
    let out = std::process::Command::new(helper)
        .args([
            "__console-list",
            &key.pid.to_string(),
            &key.create_time.to_string(),
        ])
        .creation_flags(DETACHED_PROCESS)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .map_err(|_| CoreError::Unsupported)?;
    if !out.status.success() {
        return Err(CoreError::Unsupported);
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse().ok())
        .collect())
}

/// PIDs attached to the console of `key` (run inside the helper; attaches to that console).
fn list_console(key: ProcKey) -> Result<Vec<u32>> {
    let _pin = WindowsPlatform::new()
        .pin(key, Access::QUERY)
        .map_err(|_| CoreError::Unsupported)?;
    let mut pids = vec![0u32; 1024];
    // SAFETY: console calls on our own process; `pids` is a valid buffer. Our stdout (a pipe to
    // the caller) is saved before attaching and restored after, so the list reaches the caller.
    unsafe {
        let saved = GetStdHandle(STD_OUTPUT_HANDLE).map_err(|e| os("GetStdHandle", &e))?;
        let _ = FreeConsole();
        AttachConsole(key.pid).map_err(|e| os("AttachConsole", &e))?;
        let n = GetConsoleProcessList(&mut pids) as usize;
        let _ = FreeConsole();
        let _ = SetStdHandle(STD_OUTPUT_HANDLE, saved);
        pids.truncate(n.min(pids.len()));
    }
    let me = std::process::id();
    pids.retain(|&p| p != me);
    Ok(pids)
}

/// Entry point of the console helper (args after the program name):
/// `__ctrl-c <pid> <create_time>` sends Ctrl+C; `__console-list <pid> <create_time>` prints the
/// PIDs on that console, one per line. Exit code 0 = done, 1 = failed, 2 = bad arguments.
pub fn ctrl_c_helper_main(args: impl Iterator<Item = String>) -> ExitCode {
    let a: Vec<String> = args.collect();
    let (cmd, key) = match a.as_slice() {
        [cmd, pid, ct] if cmd == "__ctrl-c" || cmd == "__console-list" => {
            match (pid.parse(), ct.parse()) {
                (Ok(pid), Ok(create_time)) => (cmd.as_str(), ProcKey { pid, create_time }),
                _ => return ExitCode::from(2),
            }
        }
        _ => return ExitCode::from(2),
    };
    let ok = if cmd == "__ctrl-c" {
        send_ctrl_c(key).is_ok()
    } else {
        list_console(key).is_ok_and(|pids| {
            let mut out = std::io::stdout().lock();
            pids.iter().all(|p| writeln!(out, "{p}").is_ok()) && out.flush().is_ok()
        })
    };
    if ok {
        ExitCode::SUCCESS
    } else {
        ExitCode::FAILURE
    }
}
