//! Scripted backend for tests: each `scan()` returns the next pushed frame.

use std::collections::{HashMap, VecDeque};

use super::{Platform, ProcEnv, RawProc, SystemStats};
use crate::statics::Statics;
use crate::{Field, ProcKey, Result};

/// Test backend that replays scripted process lists.
#[derive(Debug, Default)]
pub struct FakePlatform {
    stats: SystemStats,
    /// Wall-clock advance per scan, 100 ns ticks.
    tick_100ns: u64,
    frames: VecDeque<Vec<RawProc>>,
    last: Vec<RawProc>,
    statics: HashMap<ProcKey, Statics>,
    statics_calls: usize,
    envs: HashMap<ProcKey, ProcEnv>,
    env_calls: usize,
}

impl FakePlatform {
    /// Creates an empty fake machine with `logical_cpus` CPUs.
    /// The clock advances one second per scan and RAM is 16 GiB total / 8 GiB used.
    #[must_use]
    pub fn new(logical_cpus: u32) -> Self {
        Self {
            stats: SystemStats {
                logical_cpus,
                total_ram: 16 << 30,
                used_ram: 8 << 30,
                now_100ns: 0,
            },
            tick_100ns: 10_000_000,
            ..Self::default()
        }
    }

    /// Sets how far the fake clock advances per scan (100 ns ticks).
    pub fn set_tick(&mut self, tick_100ns: u64) {
        self.tick_100ns = tick_100ns;
    }

    /// Scripts what `statics(key)` returns; unscripted keys get all-`Unsupported`.
    pub fn set_statics(&mut self, key: ProcKey, s: Statics) {
        self.statics.insert(key, s);
    }

    /// Scripts what `cwd_and_env(key)` returns; unscripted keys are `Unsupported`.
    pub fn set_env(&mut self, key: ProcKey, env: ProcEnv) {
        self.envs.insert(key, env);
    }

    /// How many times `cwd_and_env()` was called.
    #[must_use]
    pub fn env_calls(&self) -> usize {
        self.env_calls
    }

    /// How many times `statics()` was called.
    #[must_use]
    pub fn statics_calls(&self) -> usize {
        self.statics_calls
    }

    /// Queues the process list returned by a future `scan()`.
    pub fn push_frame(&mut self, procs: Vec<RawProc>) {
        self.frames.push_back(procs);
    }
}

impl Platform for FakePlatform {
    fn scan(&mut self, out: &mut Vec<RawProc>) -> Result<SystemStats> {
        if let Some(next) = self.frames.pop_front() {
            self.last = next;
        }
        out.clear();
        out.extend(self.last.iter().cloned());
        self.stats.now_100ns += self.tick_100ns;
        Ok(self.stats)
    }

    fn statics(&mut self, key: ProcKey) -> Statics {
        self.statics_calls += 1;
        self.statics
            .get(&key)
            .cloned()
            .unwrap_or_else(|| Statics::unavailable(&Field::Unsupported))
    }

    fn cwd_and_env(&mut self, key: ProcKey) -> Field<ProcEnv> {
        self.env_calls += 1;
        self.envs
            .get(&key)
            .cloned()
            .map_or(Field::Unsupported, Field::Ok)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcKey;
    use crate::platform::{Platform, RawProc};

    fn p(pid: u32, parent: u32, name: &str) -> RawProc {
        RawProc::new(
            ProcKey {
                pid,
                create_time: u64::from(pid) * 10,
            },
            parent,
            name,
        )
    }

    #[test]
    fn fake_scan_returns_scripted_frames_in_order() {
        let mut f = FakePlatform::new(8);
        f.push_frame(vec![p(1, 0, "a")]);
        f.push_frame(vec![p(2, 0, "b"), p(3, 2, "c")]);
        let mut out = Vec::new();
        let stats = f.scan(&mut out).unwrap();
        assert_eq!(stats.logical_cpus, 8);
        assert_eq!(out, vec![p(1, 0, "a")]);
        let t1 = stats.now_100ns;
        let stats = f.scan(&mut out).unwrap();
        assert_eq!(out, vec![p(2, 0, "b"), p(3, 2, "c")]);
        assert_eq!(stats.now_100ns - t1, 10_000_000);
    }

    #[test]
    fn fake_scan_repeats_last_frame() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(1, 0, "a")]);
        let mut out = Vec::new();
        f.scan(&mut out).unwrap();
        f.scan(&mut out).unwrap();
        assert_eq!(out, vec![p(1, 0, "a")]);
    }

    #[test]
    fn fake_scan_clears_and_reuses_out_buffer() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(1, 0, "a"), p(2, 1, "b")]);
        f.push_frame(vec![p(3, 0, "c"), p(4, 3, "d")]);
        let mut out = Vec::new();
        f.scan(&mut out).unwrap();
        let cap = out.capacity();
        f.scan(&mut out).unwrap();
        assert_eq!(out.len(), 2);
        assert_eq!(out.capacity(), cap);
        assert_eq!(out[0].key.pid, 3);
    }
}
