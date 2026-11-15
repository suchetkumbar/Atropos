//! Wall-clock time on every OS (spec Phase 7): FILETIME ticks (100 ns since 1601, UTC), the
//! epoch [`crate::ProcKey::create_time`] uses everywhere, and the local date and time the UI
//! shows (formatted by the UI itself, Z4).

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

/// Now, as FILETIME ticks (UTC).
#[must_use]
pub fn now_filetime() -> u64 {
    let since = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    crate::procfs::UNIX_EPOCH_FILETIME + u64::try_from(since.as_nanos() / 100).unwrap_or(0)
}

/// FILETIME ticks (UTC) in the current time zone; `None` if out of range.
#[must_use]
pub fn local_time(filetime_100ns: u64) -> Option<LocalTime> {
    #[cfg(windows)]
    {
        crate::platform::windows::local_time(filetime_100ns)
    }
    #[cfg(unix)]
    {
        let unix = filetime_100ns.checked_sub(crate::procfs::UNIX_EPOCH_FILETIME)? / 10_000_000;
        crate::platform::unix::local_time(i64::try_from(unix).ok()?)
    }
    #[cfg(not(any(windows, unix)))]
    {
        let _ = filetime_100ns;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn now_is_after_2024_and_agrees_with_the_os() {
        let t = local_time(now_filetime()).unwrap();
        assert!(t.year >= 2024, "{t:?}");
        assert!((1..=12).contains(&t.month) && (1..=31).contains(&t.day));
        #[cfg(windows)]
        {
            let os = crate::platform::windows::now_filetime();
            assert!(os.abs_diff(now_filetime()) < 10_000_000, "within a second");
        }
    }

    #[test]
    fn utc_noon_lands_on_the_same_or_an_adjacent_local_day() {
        // 2026-01-01 12:00:00 UTC; every time zone is within ±14 h.
        let t = local_time(134_117_424_000_000_000).unwrap();
        assert!(t.year == 2026 || t.year == 2025);
        assert!(t.minute == 0 || t.minute == 30 || t.minute == 45);
        assert_eq!(t.second, 0);
    }
}
