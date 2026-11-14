//! FILETIME → local wall-clock time (for the "Started" column). Locale-independent: the GUI
//! formats the numbers itself (Z4).

use windows::Win32::Foundation::{FILETIME, SYSTEMTIME};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};

/// A local date and time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LocalTime {
    pub year: u16,
    pub month: u16,
    pub day: u16,
    pub hour: u16,
    pub minute: u16,
    pub second: u16,
}

/// Converts FILETIME ticks (UTC) to the current time zone; `None` if out of range.
#[must_use]
pub fn local_time(filetime_100ns: u64) -> Option<LocalTime> {
    #[allow(clippy::cast_possible_truncation)] // splitting a u64 into its two halves
    let ft = FILETIME {
        dwLowDateTime: filetime_100ns as u32,
        dwHighDateTime: (filetime_100ns >> 32) as u32,
    };
    let mut utc = SYSTEMTIME::default();
    let mut local = SYSTEMTIME::default();
    // SAFETY: valid pointers to stack values for the duration of each call.
    unsafe {
        FileTimeToSystemTime(&raw const ft, &raw mut utc).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &raw const utc, &raw mut local).ok()?;
    }
    Some(LocalTime {
        year: local.wYear,
        month: local.wMonth,
        day: local.wDay,
        hour: local.wHour,
        minute: local.wMinute,
        second: local.wSecond,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_utc_noon_maps_to_the_same_or_adjacent_local_day() {
        // 2026-01-01 12:00:00 UTC; every time zone is within ±14 h.
        let t = local_time(134_117_424_000_000_000).unwrap();
        assert!(t.year == 2026 || t.year == 2025);
        assert!(t.minute == 0 || t.minute == 30 || t.minute == 45);
        assert_eq!(t.second, 0);
    }

    #[test]
    fn now_is_after_2024() {
        let t = local_time(super::super::now_filetime()).unwrap();
        assert!(t.year >= 2024);
        assert!((1..=12).contains(&t.month));
    }
}
