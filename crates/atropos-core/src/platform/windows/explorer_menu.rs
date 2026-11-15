//! The opt-in, per-user Explorer context-menu entry "What is locking this?" (spec Phase 6).
//!
//! This is the **only** registry code in Atropos and the one approved exception to the
//! registry rule: it writes and deletes exactly the two keys below under `HKEY_CURRENT_USER`,
//! only when the user ticks or unticks the box in Settings. No test calls [`install`],
//! [`remove`] or [`is_installed`] (only the pure [`command_line`] is tested).

use std::path::Path;

use windows::Win32::Foundation::{ERROR_FILE_NOT_FOUND, ERROR_SUCCESS, WIN32_ERROR};
use windows::Win32::System::Registry::{
    HKEY, HKEY_CURRENT_USER, KEY_READ, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ, RegCloseKey,
    RegCreateKeyExW, RegDeleteTreeW, RegOpenKeyExW, RegSetValueExW,
};
use windows::core::{HSTRING, PCWSTR};

use crate::{CoreError, Result};

/// Files and folders (`*` covers every file type; `Directory` covers folders).
const KEYS: [&str; 2] = [
    r"Software\Classes\*\shell\Atropos.Lock",
    r"Software\Classes\Directory\shell\Atropos.Lock",
];
const TEXT: &str = "What is locking this? (Atropos)";

/// The command Explorer runs: `"<exe>" --lock "%1"` (`%1` is the clicked path).
#[must_use]
pub fn command_line(exe: &Path) -> String {
    format!("\"{}\" --lock \"%1\"", exe.display())
}

#[allow(clippy::cast_possible_wrap)] // Win32 error codes as reported
fn os(api: &'static str, e: WIN32_ERROR) -> CoreError {
    CoreError::Os {
        api,
        code: e.0 as i32,
    }
}

/// An open registry key, closed on drop.
struct Key(HKEY);

impl Drop for Key {
    fn drop(&mut self) {
        // SAFETY: `self.0` was opened or created by this module and is closed once.
        let _ = unsafe { RegCloseKey(self.0) };
    }
}

fn create(path: &str) -> Result<Key> {
    let mut h = HKEY::default();
    // SAFETY: `path` lives for the call (HSTRING); `h` is a valid out pointer.
    let r = unsafe {
        RegCreateKeyExW(
            HKEY_CURRENT_USER,
            &HSTRING::from(path),
            None,
            PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_WRITE,
            None,
            &raw mut h,
            None,
        )
    };
    if r != ERROR_SUCCESS {
        return Err(os("RegCreateKeyExW", r));
    }
    Ok(Key(h))
}

/// Sets a `REG_SZ` value (`name` empty = the key's default value).
fn set(key: &Key, name: &str, value: &str) -> Result<()> {
    let data: Vec<u8> = value
        .encode_utf16()
        .chain(Some(0))
        .flat_map(u16::to_le_bytes)
        .collect();
    // SAFETY: `key` is open for writing; `data` is a NUL-terminated UTF-16 string as bytes.
    let r = unsafe { RegSetValueExW(key.0, &HSTRING::from(name), None, REG_SZ, Some(&data)) };
    if r != ERROR_SUCCESS {
        return Err(os("RegSetValueExW", r));
    }
    Ok(())
}

/// Adds (or refreshes, after Atropos moved) the entry for files and folders, pointing at `exe`.
///
/// # Errors
/// If a key or value cannot be written; the caller then calls [`remove`] to leave nothing half
/// written.
pub fn install(exe: &Path) -> Result<()> {
    for path in KEYS {
        let verb = create(path)?;
        set(&verb, "", TEXT)?;
        set(&verb, "Icon", &format!("\"{}\",0", exe.display()))?;
        let command = create(&format!(r"{path}\command"))?;
        set(&command, "", &command_line(exe))?;
    }
    Ok(())
}

/// Deletes both entries (a missing one is fine).
///
/// # Errors
/// If a key exists but cannot be deleted.
pub fn remove() -> Result<()> {
    for path in KEYS {
        // SAFETY: `path` lives for the call (HSTRING).
        let r = unsafe { RegDeleteTreeW(HKEY_CURRENT_USER, &HSTRING::from(path)) };
        if r != ERROR_SUCCESS && r != ERROR_FILE_NOT_FOUND {
            return Err(os("RegDeleteTreeW", r));
        }
    }
    Ok(())
}

/// Whether both entries are present.
#[must_use]
pub fn is_installed() -> bool {
    KEYS.iter().all(|path| {
        let mut h = HKEY::default();
        // SAFETY: the path lives for the call (HSTRING); `h` is a valid out pointer.
        let r = unsafe {
            RegOpenKeyExW(
                HKEY_CURRENT_USER,
                &HSTRING::from(format!(r"{path}\command")),
                None,
                KEY_READ,
                &raw mut h,
            )
        };
        let ok = r == ERROR_SUCCESS;
        if ok {
            drop(Key(h));
        }
        ok
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_command_quotes_the_exe_and_the_clicked_path() {
        assert_eq!(
            command_line(Path::new(r"C:\Program Files\Atropos\Atropos.exe")),
            r#""C:\Program Files\Atropos\Atropos.exe" --lock "%1""#
        );
        assert_eq!(
            command_line(Path::new(r"D:\tööls\Atropos.exe")),
            r#""D:\tööls\Atropos.exe" --lock "%1""#
        );
    }
}
