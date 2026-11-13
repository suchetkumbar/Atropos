//! Cold-start measurement: process creation → first frame.

/// Milliseconds between two FILETIME instants (100 ns ticks); 0 if the clock went backwards.
#[must_use]
#[allow(clippy::cast_precision_loss)] // tick deltas are far below 2^52
pub fn elapsed_ms(create_time: u64, now: u64) -> f64 {
    now.saturating_sub(create_time) as f64 / 10_000.0
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
}
