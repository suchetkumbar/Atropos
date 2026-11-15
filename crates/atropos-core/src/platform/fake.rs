//! Scripted backend for tests: each `scan()` returns the next pushed frame.

use std::collections::{HashMap, HashSet, VecDeque};

use super::{Platform, ProcEnv, RawProc, SystemStats};
use crate::CoreError;
use crate::engine::kill::Strategy;
use crate::pin::{Access, Job, JobHandle, PinError, PinHandle, Pinned};
use crate::ports::{ServiceInfo, Socket};
use crate::statics::Statics;
use crate::{Field, ProcKey, Result};

/// How a fake process responds to stop requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Reaction {
    /// Exits when asked gracefully.
    HonoursGraceful,
    /// Ignores the graceful request; dies on terminate.
    #[default]
    IgnoresGraceful,
    /// The graceful request cannot be delivered; dies on terminate.
    GracefulFails,
    /// Survives even `TerminateProcess` (→ `Stuck`).
    Unkillable,
}

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
    /// Returned by `sockets()` / `services()`.
    pub sockets: Vec<Socket>,
    pub services: Vec<ServiceInfo>,
    /// Returned by `gpu()`; `None` = a machine without GPU counters.
    pub gpu: Option<Vec<crate::gpu::GpuSample>>,
    denied: HashSet<ProcKey>,
    exited: HashSet<ProcKey>,
    reactions: HashMap<ProcKey, Reaction>,
    strategies: HashMap<ProcKey, Strategy>,
    graceful_calls: usize,
    terminate_calls: usize,
    suspend_counts: HashMap<ProcKey, u32>,
    suspend_log: Vec<ProcKey>,
    on_resume: HashMap<ProcKey, Vec<RawProc>>,
    jobs: Vec<HashSet<ProcKey>>,
    consoles: HashMap<ProcKey, Vec<u32>>,
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

    /// Scripts how `key` reacts to stop requests (default: ignores graceful, dies on terminate).
    pub fn set_reaction(&mut self, key: ProcKey, r: Reaction) {
        self.reactions.insert(key, r);
    }

    /// Scripts the graceful strategy reported for `key` (default: `Strategy::None`).
    pub fn set_strategy(&mut self, key: ProcKey, s: Strategy) {
        self.strategies.insert(key, s);
    }

    /// When `parent` is resumed to a zero suspend count, `child` starts (and joins the parent's
    /// job, as Windows does for job members).
    pub fn spawn_on_resume(&mut self, parent: ProcKey, child: RawProc) {
        self.on_resume.entry(parent).or_default().push(child);
    }

    /// Scripts `console_processes(key)`.
    pub fn set_console(&mut self, key: ProcKey, pids: Vec<u32>) {
        self.consoles.insert(key, pids);
    }

    /// Keys in the order `suspend()` was called.
    #[must_use]
    pub fn suspend_log(&self) -> &[ProcKey] {
        &self.suspend_log
    }

    #[must_use]
    pub fn suspend_count(&self, key: ProcKey) -> u32 {
        self.suspend_counts.get(&key).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn graceful_calls(&self) -> usize {
        self.graceful_calls
    }

    #[must_use]
    pub fn terminate_calls(&self) -> usize {
        self.terminate_calls
    }

    /// Makes `pin(key, …)` fail with `Denied`.
    pub fn deny(&mut self, key: ProcKey) {
        self.denied.insert(key);
    }

    /// Marks `key` as exited: later scans omit it and waits report it.
    pub fn exit(&mut self, key: ProcKey) {
        self.exited.insert(key);
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
        out.extend(
            self.last
                .iter()
                .filter(|p| !self.exited.contains(&p.key))
                .cloned()
                .map(|mut p| {
                    p.suspended = self.suspend_counts.get(&p.key).copied().unwrap_or(0) > 0;
                    p
                }),
        );
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

    fn pin(&mut self, key: ProcKey, access: Access) -> std::result::Result<Pinned, PinError> {
        let live = |p: &&RawProc| !self.exited.contains(&p.key);
        match self.last.iter().filter(live).find(|p| p.key.pid == key.pid) {
            None => Err(PinError::Gone),
            Some(p) if p.key != key => Err(PinError::Reused),
            Some(_) if self.denied.contains(&key) => Err(PinError::Denied),
            Some(_) => Ok(Pinned::new(key, access, PinHandle::Fake)),
        }
    }

    fn wait_exits(&mut self, pins: &[&Pinned], _timeout: std::time::Duration) -> Vec<bool> {
        pins.iter()
            .map(|p| self.exited.contains(&p.key()))
            .collect()
    }

    fn strategy(&mut self, p: &Pinned) -> Strategy {
        self.strategies
            .get(&p.key())
            .cloned()
            .unwrap_or(Strategy::None)
    }

    fn graceful(&mut self, p: &Pinned, _how: &Strategy) -> Result<()> {
        self.graceful_calls += 1;
        match self.reactions.get(&p.key()).copied().unwrap_or_default() {
            Reaction::HonoursGraceful => {
                self.exited.insert(p.key());
                Ok(())
            }
            Reaction::GracefulFails => Err(CoreError::Unsupported),
            Reaction::IgnoresGraceful | Reaction::Unkillable => Ok(()),
        }
    }

    fn terminate(&mut self, p: &Pinned) -> Result<()> {
        self.terminate_calls += 1;
        if self.reactions.get(&p.key()) != Some(&Reaction::Unkillable) {
            self.exited.insert(p.key());
        }
        Ok(())
    }

    fn suspend(&mut self, p: &Pinned) -> Result<()> {
        *self.suspend_counts.entry(p.key()).or_default() += 1;
        self.suspend_log.push(p.key());
        Ok(())
    }

    fn resume(&mut self, p: &Pinned) -> Result<()> {
        let key = p.key();
        let n = self.suspend_counts.entry(key).or_default();
        *n = n.saturating_sub(1);
        if *n == 0 {
            for child in self.on_resume.remove(&key).unwrap_or_default() {
                for job in &mut self.jobs {
                    if job.contains(&key) {
                        job.insert(child.key);
                    }
                }
                self.last.push(child);
            }
        }
        Ok(())
    }

    fn create_job(&mut self) -> Result<Job> {
        self.jobs.push(HashSet::new());
        Ok(Job {
            handle: JobHandle::Fake(self.jobs.len() - 1),
        })
    }

    fn assign(&mut self, job: &Job, p: &Pinned) -> Result<()> {
        let JobHandle::Fake(i) = job.handle else {
            return Err(CoreError::Unsupported);
        };
        self.jobs[i].insert(p.key());
        Ok(())
    }

    fn terminate_job(&mut self, job: &Job) -> Result<()> {
        let JobHandle::Fake(i) = job.handle else {
            return Err(CoreError::Unsupported);
        };
        let members: Vec<ProcKey> = self.jobs[i].iter().copied().collect();
        for key in members {
            self.terminate_calls += 1;
            if self.reactions.get(&key) != Some(&Reaction::Unkillable) {
                self.exited.insert(key);
            }
        }
        Ok(())
    }

    fn console_processes(&mut self, key: ProcKey) -> Result<Vec<u32>> {
        self.consoles
            .get(&key)
            .cloned()
            .ok_or(CoreError::Unsupported)
    }

    fn sockets(&mut self) -> Result<Vec<Socket>> {
        Ok(self.sockets.clone())
    }

    fn gpu(&mut self) -> Result<Vec<crate::gpu::GpuSample>> {
        self.gpu.clone().ok_or(CoreError::Unsupported)
    }

    fn services(&mut self) -> Result<Vec<ServiceInfo>> {
        Ok(self.services.clone())
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
