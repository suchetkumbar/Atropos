//! CPU % (spec §4.3): `Δ(kernel+user) / (Δwall × logical_cpus)`, as a percentage of the whole machine.

/// CPU % of the whole machine between two samples; `None` when no wall time passed.
#[must_use]
#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // ticks ≪ 2^52; % fits f32
pub fn cpu_pct(
    prev_cpu_100ns: u64,
    cpu_100ns: u64,
    dwall_100ns: u64,
    logical_cpus: u32,
) -> Option<f32> {
    if dwall_100ns == 0 || logical_cpus == 0 {
        return None;
    }
    let used = cpu_100ns.saturating_sub(prev_cpu_100ns) as f64;
    Some((used / (dwall_100ns as f64 * f64::from(logical_cpus)) * 100.0) as f32)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn cpu_pct_formula() {
        // 0.5 s of CPU over 1 s wall on 4 CPUs = 12.5 % of the machine.
        assert_eq!(cpu_pct(0, 5_000_000, 10_000_000, 4), Some(12.5));
    }

    #[test]
    fn cpu_pct_none_without_wall_time() {
        assert_eq!(cpu_pct(0, 5, 0, 4), None);
        assert_eq!(cpu_pct(0, 5, 10, 0), None);
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn cpu_pct_never_negative() {
        assert_eq!(cpu_pct(100, 50, 10_000_000, 4), Some(0.0));
    }
}
