//! Per-process GPU figures (spec Phase 5) from the PDH "GPU Engine" and "GPU Process Memory"
//! counters. PDH instances carry a bare PID (`pid_1234_luid_…`), so attribution to a
//! [`ProcKey`] happens here, with a create-time check against PID reuse.

use std::collections::HashMap;

use crate::ProcKey;
use crate::snapshot::Snapshot;

/// One process's figures from one sample, keyed by the PID in the instance names.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GpuSample {
    pub pid: u32,
    /// Busiest engine, percent (Task Manager's GPU column).
    pub util_pct: f32,
    /// Dedicated GPU memory, bytes, summed over adapters.
    pub dedicated: u64,
}

/// What the GUI shows for one process.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct GpuUsage {
    pub util_pct: f32,
    pub dedicated: u64,
}

pub type GpuMap = HashMap<ProcKey, GpuUsage>;

/// The PID of a PDH GPU instance name (`pid_1234_luid_0x…_phys_0_eng_3_engtype_3D`).
#[must_use]
pub fn instance_pid(name: &str) -> Option<u32> {
    let rest = name.strip_prefix("pid_")?;
    let end = rest.find('_').unwrap_or(rest.len());
    rest[..end].parse().ok()
}

/// Folds raw counter arrays into one sample per PID: utilisation is the busiest engine
/// (capped at 100 %), memory the sum over adapters. Unparseable names and non-finite or
/// negative values are dropped; PID 0 (the idle/system bucket) is skipped.
#[must_use]
pub fn aggregate(engines: &[(String, f64)], memory: &[(String, f64)]) -> Vec<GpuSample> {
    let mut by_pid: HashMap<u32, GpuSample> = HashMap::new();
    let ok = |v: f64| v.is_finite() && v >= 0.0;
    for (name, v) in engines {
        if let Some(pid) = instance_pid(name).filter(|p| *p != 0) {
            if ok(*v) {
                #[allow(clippy::cast_possible_truncation)] // a percentage
                let v = v.min(100.0) as f32;
                let s = by_pid.entry(pid).or_insert(GpuSample {
                    pid,
                    ..GpuSample::default()
                });
                s.util_pct = s.util_pct.max(v);
            }
        }
    }
    for (name, v) in memory {
        if let Some(pid) = instance_pid(name).filter(|p| *p != 0) {
            if ok(*v) {
                #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // bytes ≥ 0
                let v = *v as u64;
                let s = by_pid.entry(pid).or_insert(GpuSample {
                    pid,
                    ..GpuSample::default()
                });
                s.dedicated = s.dedicated.saturating_add(v);
            }
        }
    }
    let mut out: Vec<GpuSample> = by_pid.into_values().collect();
    out.sort_by_key(|s| s.pid);
    out
}

/// Maps samples to processes. Utilisation is a rate PDH computes between two collections; if a
/// PID now belongs to a different process than at the previous sample, that rate mixes two
/// processes, so the PID is dropped for this sample (memory too, to keep the row consistent).
#[derive(Debug, Default)]
pub struct Attribution {
    /// The process each PID belonged to at the previous sample.
    last: HashMap<u32, ProcKey>,
}

impl Attribution {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets the previous sample (sampling stopped; the next one starts fresh).
    pub fn reset(&mut self) {
        self.last.clear();
    }

    pub fn attribute(&mut self, samples: &[GpuSample], snap: &Snapshot) -> GpuMap {
        let by_pid: HashMap<u32, ProcKey> = snap
            .procs
            .iter()
            .map(|p| (p.raw.key.pid, p.raw.key))
            .collect();
        let mut out = GpuMap::with_capacity(samples.len());
        for s in samples {
            let Some(&key) = by_pid.get(&s.pid) else {
                continue;
            };
            if self.last.get(&s.pid).is_some_and(|prev| *prev != key) {
                continue;
            }
            out.insert(
                key,
                GpuUsage {
                    util_pct: s.util_pct,
                    dedicated: s.dedicated,
                },
            );
        }
        self.last = by_pid;
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{RawProc, SystemStats};
    use crate::snapshot::build;

    fn s(name: &str, v: f64) -> (String, f64) {
        (name.to_owned(), v)
    }

    fn snap(procs: &[(u32, u64)]) -> Snapshot {
        let raw = procs
            .iter()
            .map(|&(pid, ct)| {
                RawProc::new(
                    ProcKey {
                        pid,
                        create_time: ct,
                    },
                    0,
                    "x.exe",
                )
            })
            .collect();
        build(None, raw, SystemStats::default())
    }

    #[test]
    fn instance_names_yield_their_pid() {
        assert_eq!(
            instance_pid("pid_1234_luid_0x00000000_0x0000D1E3_phys_0_eng_0_engtype_3D"),
            Some(1234)
        );
        assert_eq!(instance_pid("pid_77_luid_0x0_0x1_phys_0"), Some(77));
        assert_eq!(instance_pid("pid_88"), Some(88));
        assert_eq!(instance_pid("_Total"), None);
        assert_eq!(instance_pid("pid_x_luid"), None);
        assert_eq!(instance_pid(""), None);
    }

    #[test]
    fn utilisation_is_the_busiest_engine_and_memory_sums_over_adapters() {
        let engines = [
            s("pid_10_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 30.0),
            s("pid_10_luid_0x0_0x1_phys_0_eng_1_engtype_Copy", 5.0),
            s("pid_10_luid_0x0_0x1_phys_0_eng_2_engtype_VideoDecode", 42.5),
            s("pid_20_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 250.0),
        ];
        let memory = [
            s("pid_10_luid_0x0_0x1_phys_0", 1000.0),
            s("pid_10_luid_0x0_0x2_phys_0", 24.0),
            s("pid_30_luid_0x0_0x1_phys_0", 7.0),
        ];
        assert_eq!(
            aggregate(&engines, &memory),
            vec![
                GpuSample {
                    pid: 10,
                    util_pct: 42.5,
                    dedicated: 1024
                },
                GpuSample {
                    pid: 20,
                    util_pct: 100.0,
                    dedicated: 0
                },
                GpuSample {
                    pid: 30,
                    util_pct: 0.0,
                    dedicated: 7
                },
            ]
        );
    }

    #[test]
    fn junk_names_values_and_pid_zero_are_dropped() {
        let engines = [
            s("garbage", 50.0),
            s("pid_0_luid_0x0_0x1_phys_0_eng_0_engtype_3D", 80.0),
            s("pid_5_luid_0x0_0x1_phys_0_eng_0_engtype_3D", f64::NAN),
            s("pid_6_luid_0x0_0x1_phys_0_eng_0_engtype_3D", -1.0),
        ];
        assert_eq!(aggregate(&engines, &[]), Vec::new());
    }

    #[test]
    fn samples_are_attributed_by_pid_to_the_current_process() {
        let mut a = Attribution::new();
        let got = a.attribute(
            &[
                GpuSample {
                    pid: 10,
                    util_pct: 12.0,
                    dedicated: 5,
                },
                GpuSample {
                    pid: 99,
                    util_pct: 1.0,
                    dedicated: 1,
                },
            ],
            &snap(&[(10, 1)]),
        );
        let key = ProcKey {
            pid: 10,
            create_time: 1,
        };
        assert_eq!(got.len(), 1, "a PID with no process is dropped");
        assert_eq!(
            got[&key],
            GpuUsage {
                util_pct: 12.0,
                dedicated: 5
            }
        );
    }

    #[test]
    fn a_reused_pid_is_not_credited_with_the_old_process_rate() {
        let mut a = Attribution::new();
        let sample = [GpuSample {
            pid: 10,
            util_pct: 90.0,
            dedicated: 5,
        }];
        assert_eq!(a.attribute(&sample, &snap(&[(10, 1)])).len(), 1);
        // PID 10 now belongs to a new process (create time 2): dropped once …
        assert!(a.attribute(&sample, &snap(&[(10, 2)])).is_empty());
        // … then attributed normally.
        let got = a.attribute(&sample, &snap(&[(10, 2)]));
        assert!(got.contains_key(&ProcKey {
            pid: 10,
            create_time: 2
        }));
    }

    #[test]
    fn reset_forgets_the_previous_owner() {
        let mut a = Attribution::new();
        let sample = [GpuSample {
            pid: 10,
            util_pct: 1.0,
            dedicated: 1,
        }];
        let _ = a.attribute(&sample, &snap(&[(10, 1)]));
        a.reset();
        assert_eq!(a.attribute(&sample, &snap(&[(10, 2)])).len(), 1);
    }
}
