//! Scripted backend for tests: each `scan()` returns the next pushed frame.

use std::collections::VecDeque;

use super::{Platform, RawProc, SystemStats};
use crate::Result;

/// Test backend that replays scripted process lists.
#[derive(Debug, Default)]
pub struct FakePlatform {
    logical_cpus: u32,
    frames: VecDeque<Vec<RawProc>>,
    last: Vec<RawProc>,
}

impl FakePlatform {
    /// Creates an empty fake machine with `logical_cpus` CPUs.
    #[must_use]
    pub fn new(logical_cpus: u32) -> Self {
        Self {
            logical_cpus,
            ..Self::default()
        }
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
        Ok(SystemStats {
            logical_cpus: self.logical_cpus,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcKey;
    use crate::platform::{Platform, RawProc};
    use std::sync::Arc;

    fn p(pid: u32, parent: u32, name: &str) -> RawProc {
        RawProc {
            key: ProcKey {
                pid,
                create_time: u64::from(pid) * 10,
            },
            ppid: parent,
            image: Arc::from(name),
        }
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
        f.scan(&mut out).unwrap();
        assert_eq!(out, vec![p(2, 0, "b"), p(3, 2, "c")]);
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
