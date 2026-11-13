//! Native message box for fatal errors in the GUI-subsystem binary (which has no console).

use windows::Win32::UI::WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW};
use windows::core::HSTRING;

/// Shows a blocking, owner-less error dialog. Errors from the dialog itself are ignored.
pub fn fatal_message(title: &str, body: &str) {
    let title = HSTRING::from(title);
    let body = HSTRING::from(body);
    // SAFETY: both HSTRINGs are NUL-terminated UTF-16 and outlive the call; a null owner window is allowed.
    unsafe {
        MessageBoxW(None, &body, &title, MB_OK | MB_ICONERROR);
    }
}
