//! Small Unix helpers shared by the Linux and macOS backends: local time (`localtime_r(3)`,
//! which applies `TZ` and `/etc/localtime`) and the effective user.

use crate::time::LocalTime;

/// Seconds since the Unix epoch (UTC) in the current time zone.
pub(crate) fn local_time(unix_secs: i64) -> Option<LocalTime> {
    #[allow(clippy::useless_conversion)] // `time_t` is 32-bit on some targets
    let t: libc::time_t = unix_secs.try_into().ok()?;
    // SAFETY: plain old data; all-zero is a valid `tm`.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    // SAFETY: both pointers are valid for the call; `localtime_r` is the thread-safe form.
    if unsafe { libc::localtime_r(&raw const t, &raw mut tm) }.is_null() {
        return None;
    }
    let n = |v: libc::c_int| u16::try_from(v).ok();
    Some(LocalTime {
        year: n(tm.tm_year + 1900)?,
        month: n(tm.tm_mon + 1)?,
        day: n(tm.tm_mday)?,
        hour: n(tm.tm_hour)?,
        minute: n(tm.tm_min)?,
        second: n(tm.tm_sec)?,
    })
}

/// Whether the process runs as root (effective UID 0).
pub(crate) fn effective_root() -> bool {
    // SAFETY: no preconditions.
    unsafe { libc::geteuid() == 0 }
}
