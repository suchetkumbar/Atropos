//! Cell text. Locale-independent on purpose (Z4): fixed `.` decimals, ISO dates, 24-hour time.

use atropos_core::time::LocalTime;

const KIB: f64 = 1024.0;
const MIB: f64 = KIB * 1024.0;
const GIB: f64 = MIB * 1024.0;

/// Memory, Task Manager style (1024-based): `512 KB`, `12.3 MB`, `1.25 GB`.
#[must_use]
#[allow(clippy::cast_precision_loss)] // display only
pub fn bytes(b: u64) -> String {
    let v = b as f64;
    if v < MIB {
        format!("{:.0} KB", v / KIB)
    } else if v < GIB {
        format!("{:.1} MB", v / MIB)
    } else {
        format!("{:.2} GB", v / GIB)
    }
}

/// A rate in bytes per second: `—` before the second sample.
#[must_use]
pub fn rate(r: Option<f32>) -> String {
    let Some(r) = r else {
        return "—".to_owned();
    };
    let v = f64::from(r.max(0.0));
    if v < KIB {
        format!("{v:.0} B/s")
    } else if v < MIB {
        format!("{:.0} KB/s", v / KIB)
    } else if v < GIB {
        format!("{:.1} MB/s", v / MIB)
    } else {
        format!("{:.2} GB/s", v / GIB)
    }
}

/// CPU %, one decimal: `—` before the second sample.
#[must_use]
pub fn pct(p: Option<f32>) -> String {
    p.map_or_else(|| "—".to_owned(), |v| format!("{v:.1}"))
}

/// Start time: `HH:MM:SS` if it was today, else `YYYY-MM-DD HH:MM`.
#[must_use]
pub fn started(t: LocalTime, today: LocalTime) -> String {
    if (t.year, t.month, t.day) == (today.year, today.month, today.day) {
        format!("{:02}:{:02}:{:02}", t.hour, t.minute, t.second)
    } else {
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            t.year, t.month, t.day, t.hour, t.minute
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bytes_scales() {
        assert_eq!(bytes(0), "0 KB");
        assert_eq!(bytes(512 * 1024), "512 KB");
        assert_eq!(bytes(12 * 1024 * 1024 + 300 * 1024), "12.3 MB");
        assert_eq!(bytes(1024 * 1024 * 1024 + 256 * 1024 * 1024), "1.25 GB");
    }

    #[test]
    fn rate_formats_and_dashes() {
        assert_eq!(rate(None), "—");
        assert_eq!(rate(Some(0.0)), "0 B/s");
        assert_eq!(rate(Some(900.0)), "900 B/s");
        assert_eq!(rate(Some(2.5 * 1024.0 * 1024.0)), "2.5 MB/s");
    }

    #[test]
    fn pct_none_is_dash() {
        assert_eq!(pct(None), "—");
        assert_eq!(pct(Some(12.345)), "12.3");
        assert_eq!(pct(Some(0.0)), "0.0");
    }

    fn lt(year: u16, month: u16, day: u16, hour: u16, minute: u16, second: u16) -> LocalTime {
        LocalTime {
            year,
            month,
            day,
            hour,
            minute,
            second,
        }
    }

    #[test]
    fn started_today_shows_time_only() {
        let today = lt(2026, 11, 14, 18, 0, 0);
        assert_eq!(started(lt(2026, 11, 14, 9, 5, 7), today), "09:05:07");
        assert_eq!(
            started(lt(2026, 11, 13, 23, 59, 0), today),
            "2026-11-13 23:59"
        );
    }
}
