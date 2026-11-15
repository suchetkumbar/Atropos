//! Priority, CPU affinity and efficiency mode (spec Phase 6), always through a pinned handle.
//! Policy applies: hard-blocked targets (system, Atropos and its ancestors, protected
//! processes) are refused; "risky" targets are allowed, since nothing is ended.

use crate::engine::policy::{self, ActionKind, Verdict};
use crate::pin::Access;
use crate::platform::Platform;
use crate::{CoreError, ProcKey};

/// Windows priority classes. `Realtime` is only ever *read*: Atropos does not set it (it can
/// starve the input and the disk of the whole machine).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Idle,
    BelowNormal,
    Normal,
    AboveNormal,
    High,
    Realtime,
}

impl Priority {
    /// The classes the user can choose, lowest first.
    pub const SETTABLE: [Priority; 5] = [
        Priority::Idle,
        Priority::BelowNormal,
        Priority::Normal,
        Priority::AboveNormal,
        Priority::High,
    ];

    #[must_use]
    pub fn text(self) -> &'static str {
        match self {
            Priority::Idle => "Low",
            Priority::BelowNormal => "Below normal",
            Priority::Normal => "Normal",
            Priority::AboveNormal => "Above normal",
            Priority::High => "High",
            Priority::Realtime => "Realtime",
        }
    }
}

/// One change.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tune {
    Priority(Priority),
    /// Bit n = logical CPU n (one processor group, at most 64 CPUs).
    Affinity(u64),
    /// Efficiency mode (EcoQoS): `true` throttles, `false` lets Windows decide again.
    Eco(bool),
}

/// The current settings of a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Tuning {
    pub priority: Priority,
    pub affinity: u64,
    /// The CPUs the system offers.
    pub system_affinity: u64,
    /// `None` where Windows cannot report it (before Windows 11).
    pub eco: Option<bool>,
}

/// The access a pin needs to read and change these settings.
pub const ACCESS: Access = Access {
    terminate: false,
    suspend: false,
    tune: true,
};

/// Parses `0-3,6` into a mask for a machine with `cpus` logical CPUs.
///
/// # Errors
/// A message for the user: bad syntax, a CPU the machine does not have, an empty set, or a
/// machine with more than 64 CPUs (processor groups; a single mask cannot describe it).
pub fn parse_affinity(text: &str, cpus: u32) -> Result<u64, String> {
    if cpus > 64 {
        return Err(format!(
            "This machine has {cpus} logical CPUs in several processor groups; Atropos can set \
             affinity only on machines with up to 64."
        ));
    }
    let last = cpus.saturating_sub(1);
    let range = || format!("CPUs are numbered 0-{last}");
    let mut mask = 0u64;
    for part in text.split(',').map(str::trim).filter(|p| !p.is_empty()) {
        let (a, b) = match part.split_once('-') {
            Some((a, b)) => (a.trim(), b.trim()),
            None => (part, part),
        };
        let bad = || format!("{part:?} is not a CPU number or range like 0-3");
        let a: u32 = a.parse().map_err(|_| bad())?;
        let b: u32 = b.parse().map_err(|_| bad())?;
        if a > b {
            return Err(bad());
        }
        if b > last {
            return Err(range());
        }
        for cpu in a..=b {
            mask |= 1u64 << cpu;
        }
    }
    if mask == 0 {
        return Err("Choose at least one CPU.".into());
    }
    Ok(mask)
}

/// `0-3,6` for a mask.
#[must_use]
pub fn format_affinity(mask: u64) -> String {
    let mut parts = Vec::new();
    let mut cpu = 0u32;
    while cpu < 64 {
        if mask & (1u64 << cpu) == 0 {
            cpu += 1;
            continue;
        }
        let start = cpu;
        while cpu + 1 < 64 && mask & (1u64 << (cpu + 1)) != 0 {
            cpu += 1;
        }
        parts.push(if start == cpu {
            start.to_string()
        } else {
            format!("{start}-{cpu}")
        });
        cpu += 1;
    }
    parts.join(",")
}

/// Applies `t` to `key` after the policy check.
///
/// # Errors
/// The message to show; nothing was changed.
pub fn apply(
    platform: &mut dyn Platform,
    ctx: &policy::Context<'_>,
    key: ProcKey,
    t: Tune,
) -> Result<(), String> {
    match t {
        Tune::Priority(Priority::Realtime) => {
            return Err(
                "Atropos does not set Realtime priority: it can freeze the whole \
                        machine."
                    .into(),
            );
        }
        Tune::Affinity(0) => return Err("Choose at least one CPU.".into()),
        _ => {}
    }
    // Nothing is ended, so only hard blocks refuse (the suspend rules are the closest fit).
    if let Verdict::Hard(h) = policy::check(ctx, key, ActionKind::Suspend) {
        return Err(format!("Atropos will not change this process ({h:?})."));
    }
    let pin = platform.pin(key, ACCESS).map_err(|e| match e {
        crate::pin::PinError::Denied => {
            "Windows refused (it runs as another user or elevated).".to_owned()
        }
        other => format!("Could not open the process ({other})."),
    })?;
    platform.tune(&pin, t).map_err(|e| match (t, e) {
        (Tune::Affinity(_), CoreError::Unsupported) => {
            "This machine has more than 64 logical CPUs (processor groups); affinity cannot \
             be set here."
                .to_owned()
        }
        (Tune::Eco(_), CoreError::Os { .. }) => {
            "Windows refused efficiency mode (it needs Windows 11 for full support).".to_owned()
        }
        (_, e) => format!("Windows refused ({e})."),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc};
    use crate::snapshot::build;
    use crate::statics::StaticsMap;

    #[test]
    fn affinity_text_round_trips() {
        assert_eq!(parse_affinity("0-3,6", 8), Ok(0b0100_1111));
        assert_eq!(parse_affinity(" 2 ", 8), Ok(0b100));
        assert_eq!(parse_affinity("0-63", 64), Ok(u64::MAX));
        assert_eq!(format_affinity(0b0100_1111), "0-3,6");
        assert_eq!(format_affinity(0b1010), "1,3");
        assert_eq!(format_affinity(u64::MAX), "0-63");
    }

    #[test]
    fn bad_affinity_text_is_refused_with_a_reason() {
        assert!(parse_affinity("", 8).unwrap_err().contains("at least one"));
        assert!(parse_affinity("8", 8).unwrap_err().contains("0-7"));
        assert!(parse_affinity("3-1", 8).is_err());
        assert!(parse_affinity("a,b", 8).is_err());
        assert!(parse_affinity("0", 128).unwrap_err().contains("64"));
    }

    fn setup() -> (FakePlatform, crate::snapshot::Snapshot, ProcKey, ProcKey) {
        let k = |pid| ProcKey {
            pid,
            create_time: u64::from(pid),
        };
        let mut f = FakePlatform::new(8);
        let procs = vec![
            RawProc::new(k(3), 1, "atropos.exe"),
            RawProc::new(k(10), 1, "node.exe"),
        ];
        f.push_frame(procs.clone());
        let mut raw = Vec::new();
        let stats = f.scan(&mut raw).unwrap();
        (f, build(None, raw, stats), k(3), k(10))
    }

    #[test]
    fn apply_changes_an_allowed_process_through_a_pin() {
        let (mut f, snap, me, node) = setup();
        let statics = StaticsMap::new();
        let ctx = policy::Context {
            snapshot: &snap,
            statics: &statics,
            services: &[],
            me,
        };
        apply(&mut f, &ctx, node, Tune::Priority(Priority::BelowNormal)).unwrap();
        apply(&mut f, &ctx, node, Tune::Affinity(0b11)).unwrap();
        apply(&mut f, &ctx, node, Tune::Eco(true)).unwrap();
        let p = f.pin(node, ACCESS).unwrap();
        let t = f.tuning(&p).unwrap();
        assert_eq!(t.priority, Priority::BelowNormal);
        assert_eq!(t.affinity, 0b11);
        assert_eq!(t.eco, Some(true));
    }

    #[test]
    fn apply_refuses_atropos_itself_and_realtime() {
        let (mut f, snap, me, node) = setup();
        let statics = StaticsMap::new();
        let ctx = policy::Context {
            snapshot: &snap,
            statics: &statics,
            services: &[],
            me,
        };
        assert!(apply(&mut f, &ctx, me, Tune::Eco(true)).is_err());
        assert!(
            apply(&mut f, &ctx, node, Tune::Priority(Priority::Realtime))
                .unwrap_err()
                .contains("Realtime")
        );
        assert!(apply(&mut f, &ctx, node, Tune::Affinity(0)).is_err());
    }
}
