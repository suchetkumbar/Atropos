//! Small Win32 pieces the test fixtures need (fixtures may not contain `unsafe`).

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows::Win32::System::Console::SetConsoleCtrlHandler;
use windows::Win32::UI::WindowsAndMessaging::{
    CreateWindowExW, DefWindowProcW, DestroyWindow, DispatchMessageW, GetMessageW, MSG,
    PostQuitMessage, RegisterClassW, SetTimer, WINDOW_EX_STYLE, WM_CLOSE, WM_DESTROY, WM_TIMER,
    WNDCLASSW, WS_OVERLAPPEDWINDOW, WS_VISIBLE,
};
use windows::core::{HSTRING, PCWSTR};

use crate::{CoreError, Result};

/// Makes this console process ignore Ctrl+C.
///
/// # Errors
/// If `SetConsoleCtrlHandler` fails.
pub fn ignore_ctrl_c() -> Result<()> {
    // SAFETY: no pointers; `None` + `true` = ignore Ctrl+C in this process.
    unsafe { SetConsoleCtrlHandler(None, true) }.map_err(|e| CoreError::Os {
        api: "SetConsoleCtrlHandler",
        code: e.code().0 & 0xFFFF,
    })
}

/// Restores default Ctrl+C handling (clears an ignore flag inherited from the parent), like a
/// process started from an ordinary terminal.
///
/// # Errors
/// If `SetConsoleCtrlHandler` fails.
pub fn default_ctrl_c() -> Result<()> {
    // SAFETY: no pointers; `None` + `false` = default Ctrl+C processing.
    unsafe { SetConsoleCtrlHandler(None, false) }.map_err(|e| CoreError::Os {
        api: "SetConsoleCtrlHandler",
        code: e.code().0 & 0xFFFF,
    })
}

unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, w: WPARAM, l: LPARAM) -> LRESULT {
    // SAFETY: called by the system on this thread with a valid window.
    unsafe {
        match msg {
            WM_CLOSE => {
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, w, l),
        }
    }
}

/// Shows one visible, unowned top-level window (off-screen) and pumps messages until it is
/// closed (`WM_CLOSE`) or `secs` pass.
///
/// # Errors
/// If the window cannot be created.
pub fn run_window(title: &str, secs: u64) -> Result<()> {
    let class = HSTRING::from("AtroposTestWindow");
    let name = HSTRING::from(title);
    let wc = WNDCLASSW {
        lpfnWndProc: Some(wndproc),
        lpszClassName: PCWSTR(class.as_ptr()),
        ..Default::default()
    };
    // SAFETY: `wc`, `class` and `name` outlive the calls; the window lives on this thread.
    unsafe {
        RegisterClassW(&raw const wc);
        CreateWindowExW(
            WINDOW_EX_STYLE(0),
            &class,
            &name,
            WS_OVERLAPPEDWINDOW | WS_VISIBLE,
            -3000,
            -3000,
            200,
            100,
            None,
            None,
            None,
            None,
        )
        .map_err(|e| CoreError::Os {
            api: "CreateWindowExW",
            code: e.code().0 & 0xFFFF,
        })?;
        let ms = u32::try_from(secs.saturating_mul(1000)).unwrap_or(u32::MAX);
        SetTimer(None, 0, ms, None);
        let mut msg = MSG::default();
        while GetMessageW(&raw mut msg, None, 0, 0).as_bool() {
            if msg.message == WM_TIMER {
                break;
            }
            DispatchMessageW(&raw const msg);
        }
    }
    Ok(())
}
