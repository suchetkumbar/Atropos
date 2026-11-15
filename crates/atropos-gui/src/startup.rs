//! Cold-start measurement: process creation → first frame.

/// Milliseconds between two FILETIME instants (100 ns ticks); 0 if the clock went backwards.
#[must_use]
#[allow(clippy::cast_precision_loss)] // tick deltas are far below 2^52
pub fn elapsed_ms(create_time: u64, now: u64) -> f64 {
    now.saturating_sub(create_time) as f64 / 10_000.0
}

/// The `--startup-log` line perf.ps1 reads for the idle-memory budget (ADR 0005).
#[must_use]
#[allow(clippy::cast_precision_loss)] // heap sizes are far below 2^52
#[cfg_attr(not(windows), allow(dead_code))] // the heap is counted on Windows only
pub fn heap_line(live_bytes: usize) -> String {
    format!("app_heap_mb={:.1}", live_bytes as f64 / (1024.0 * 1024.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn elapsed_ms_converts_100ns_to_ms() {
        assert_eq!(elapsed_ms(0, 3_000_000), 300.0);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn elapsed_ms_saturates_on_clock_skew() {
        assert_eq!(elapsed_ms(10, 5), 0.0);
    }

    #[test]
    fn heap_line_reports_mebibytes() {
        assert_eq!(heap_line(40 * 1024 * 1024 + 512 * 1024), "app_heap_mb=40.5");
    }
}
