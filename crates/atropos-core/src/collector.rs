//! The collector thread (spec §3.3): scan → snapshot → statics → one `Arc<Frame>` per tick over a
//! `bounded(1)` channel that keeps only the newest frame. The UI steers it with [`Interest`].

use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, TrySendError};

use crate::Field;
use crate::ProcKey;
use crate::context::{AsyncDirs, ContextCache, ContextMap, DirLookup, SyncDirs};
use crate::gpu::{Attribution, GpuMap};
use crate::labels::{Labels, RuleDef};
use crate::platform::{Platform, ProcEnv, RawProc};
use crate::ports::{Socket, TcpState};
use crate::ring::Ring;
use crate::snapshot::{Snapshot, build};
use crate::statics::{StaticsCache, StaticsMap};

/// Sparkline length (spec: 120 samples).
pub const HISTORY: usize = 120;
/// User-configurable tick range (spec §3.3).
pub const MIN_TICK: Duration = Duration::from_millis(500);
pub const MAX_TICK: Duration = Duration::from_secs(10);

/// How visible the GUI is; drives the adaptive tick.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Visibility {
    #[default]
    Focused,
    Visible,
    Minimized,
}

/// What the UI currently needs from the collector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[allow(clippy::struct_excessive_bools)] // independent switches, not a state machine
pub struct Interest {
    pub visibility: Visibility,
    /// User-chosen tick; clamped to 0.5–10 s.
    pub tick_override: Option<Duration>,
    pub selected: Option<ProcKey>,
    /// Scan now instead of waiting for the tick (`F5`).
    pub refresh_now: bool,
    /// Read sockets every tick (a `:port` search or the Ports tab is active).
    pub want_ports: bool,
    /// Resolve the repository of every process (the Projects view).
    pub want_projects: bool,
    /// Read sockets and resolve the repository of every listener (the Leftovers view).
    pub want_leftovers: bool,
    /// Sample GPU counters (GPU columns or the GPU treemap are visible).
    pub want_gpu: bool,
}

/// Tick for this interest: 1 s focused, 2 s visible, 5 s minimized, or the clamped override.
#[must_use]
pub fn tick_for(i: &Interest) -> Duration {
    match (i.visibility, i.tick_override) {
        (Visibility::Minimized, _) => Duration::from_secs(5),
        (_, Some(t)) => t.clamp(MIN_TICK, MAX_TICK),
        (Visibility::Focused, None) => Duration::from_secs(1),
        (Visibility::Visible, None) => Duration::from_secs(2),
    }
}

/// Everything the UI renders for one tick.
#[derive(Debug, Clone)]
pub struct Frame {
    pub snapshot: Snapshot,
    pub statics: Arc<StaticsMap>,
    /// Whole-machine CPU % and used-RAM %, newest last.
    pub cpu_history: Ring<HISTORY>,
    pub mem_history: Ring<HISTORY>,
    /// How long this tick's scan + build + statics took.
    pub tick_ms: f32,
    /// The part of `tick_ms` spent in the OS calls `Platform::scan` and `Platform::gpu` (spec
    /// §3.3). The budget (≤ 5 ms p95) applies to `tick_ms - scan_ms`; OS time is tracked
    /// separately (ADR 0007).
    pub scan_ms: f32,
    /// Working directory and environment of `Interest::selected`, read once per selection
    /// (and again on `refresh_now`).
    pub env: Option<(ProcKey, Arc<Field<ProcEnv>>)>,
    /// All sockets, read this tick when `Interest::want_ports` or `want_leftovers` (else `None`).
    pub sockets: Option<Arc<Vec<Socket>>>,
    /// Smart labels, working directories and repositories (spec §4.8).
    pub context: Arc<ContextMap>,
    /// CPU % and memory (MiB) history of `Interest::selected`, newest last; restarts when the
    /// selection changes. Kept for the selected process only, so memory stays bounded.
    pub history: Option<(ProcKey, Ring<HISTORY>, Ring<HISTORY>)>,
    /// GPU figures per process, when `Interest::want_gpu` and the machine has GPU counters
    /// (else `None`, shown as "—").
    pub gpu: Option<Arc<GpuMap>>,
}

/// Sends `item`, first discarding a frame the UI has not taken yet (drop-oldest). `drain` is the
/// collector's own clone of the receiver, so the channel never disconnects: the collector stops
/// when the UI drops its [`Interest`] sender instead.
pub fn send_latest<T>(tx: &Sender<T>, drain: &Receiver<T>, item: T) {
    if let Err(TrySendError::Full(item)) = tx.try_send(item) {
        let _ = drain.try_recv();
        // Only this thread sends, so after the drain there is room.
        let _ = tx.try_send(item);
    }
}

/// A running collector. Dropping `interest` stops the thread at its next wake-up.
pub struct Collector {
    pub interest: Sender<Interest>,
    pub frames: Receiver<Arc<Frame>>,
    pub thread: JoinHandle<()>,
}

/// Called after each new frame is sent (the GUI requests a repaint here).
pub type Notify = Box<dyn Fn() + Send>;

/// Starts the collector thread on `platform`; `notify` runs after every frame.
///
/// # Errors
/// If the OS refuses to start a thread.
pub fn spawn(platform: Box<dyn Platform>, notify: Notify) -> std::io::Result<Collector> {
    spawn_with(platform, notify, Vec::new())
}

/// Like [`spawn`], with the user's label rules (`[[labels]]` in `config.toml`) ahead of the
/// built-ins. The rules are compiled on the collector thread.
///
/// # Errors
/// If the OS refuses to start a thread.
pub fn spawn_with(
    platform: Box<dyn Platform>,
    notify: Notify,
    user_rules: Vec<RuleDef>,
) -> std::io::Result<Collector> {
    let (interest_tx, interest_rx) = crossbeam_channel::unbounded();
    let (frame_tx, frame_rx) = crossbeam_channel::bounded(1);
    let drain = frame_rx.clone();
    let thread = std::thread::Builder::new()
        .name("atropos-collector".into())
        .spawn(move || {
            run(
                platform,
                &interest_rx,
                &frame_tx,
                &drain,
                &notify,
                &user_rules,
            );
        })?;
    Ok(Collector {
        interest: interest_tx,
        frames: frame_rx,
        thread,
    })
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // RAM byte counts ≪ 2^52 for a percentage
fn mem_pct(used: u64, total: u64) -> f32 {
    if total == 0 {
        0.0
    } else {
        (used as f64 / total as f64 * 100.0) as f32
    }
}

#[allow(clippy::cast_precision_loss, clippy::cast_possible_truncation)] // display-only
fn mib(bytes: u64) -> f32 {
    (bytes as f64 / (1024.0 * 1024.0)) as f32
}

#[allow(clippy::too_many_lines)]
fn run(
    mut platform: Box<dyn Platform>,
    interest_rx: &Receiver<Interest>,
    frame_tx: &Sender<Arc<Frame>>,
    drain: &Receiver<Arc<Frame>>,
    notify: &Notify,
    user_rules: &[RuleDef],
) {
    let mut ctx = ContextCache::new(Labels::with_user(user_rules).0);
    let mut dirs: Box<dyn DirLookup> = match AsyncDirs::spawn(ctx.markers()) {
        Ok(d) => Box::new(d),
        Err(e) => {
            tracing::warn!("directory prober: {e}");
            Box::new(SyncDirs::new(ctx.markers()))
        }
    };
    let mut context = Arc::new(ContextMap::new());
    let mut history: Option<(ProcKey, Ring<HISTORY>, Ring<HISTORY>)> = None;
    let mut interest = Interest::default();
    let mut cache = StaticsCache::new();
    let mut statics = Arc::new(StaticsMap::new());
    let mut prev: Option<Arc<Frame>> = None;
    let mut env: Option<(ProcKey, Arc<Field<ProcEnv>>)> = None;
    let mut env_stale = false;
    let mut attribution = Attribution::new();
    let mut gpu_on = false;
    loop {
        let started = Instant::now();
        let mut raw: Vec<RawProc> =
            Vec::with_capacity(prev.as_ref().map_or(256, |f| f.snapshot.procs.len() + 32));
        let scanned = platform.scan(&mut raw);
        let mut scan_ms = started.elapsed().as_secs_f32() * 1000.0;
        match scanned {
            Ok(stats) => {
                let snapshot = build(prev.as_ref().map(|f| &f.snapshot), raw, stats);
                if cache.refresh(platform.as_mut(), &snapshot) {
                    statics = Arc::new(cache.map().clone());
                }
                env = match interest.selected {
                    Some(k) if env_stale || env.as_ref().is_none_or(|(ek, _)| *ek != k) => {
                        Some((k, Arc::new(platform.cwd_and_env(k))))
                    }
                    Some(_) => env,
                    None => None,
                };
                env_stale = false;
                let gpu = if interest.want_gpu {
                    let t = Instant::now();
                    let samples = platform.gpu();
                    scan_ms += t.elapsed().as_secs_f32() * 1000.0;
                    match samples {
                        Ok(s) => Some(Arc::new(attribution.attribute(&s, &snapshot))),
                        Err(e) => {
                            if !gpu_on {
                                tracing::debug!("gpu: {e}");
                            }
                            None
                        }
                    }
                } else {
                    if gpu_on {
                        platform.gpu_stop();
                        attribution.reset();
                    }
                    None
                };
                gpu_on = interest.want_gpu;
                let sockets = if interest.want_ports || interest.want_leftovers {
                    platform
                        .sockets()
                        .inspect_err(|e| tracing::warn!("sockets: {e}"))
                        .ok()
                        .map(Arc::new)
                } else {
                    None
                };
                let listeners: Vec<u32> = match (&sockets, interest.want_leftovers) {
                    (Some(all), true) => all
                        .iter()
                        .filter(|s| s.state == Some(TcpState::Listen))
                        .map(|s| s.owner)
                        .collect(),
                    _ => Vec::new(),
                };
                let want_cwd = |p: &crate::snapshot::Proc| {
                    interest.want_projects || listeners.contains(&p.raw.key.pid)
                };
                if ctx.refresh(
                    platform.as_mut(),
                    &snapshot,
                    &statics,
                    dirs.as_mut(),
                    &want_cwd,
                ) {
                    context = Arc::new(ctx.map().clone());
                }
                history =
                    interest.selected.and_then(|k| {
                        let p = snapshot.get(k)?;
                        let (_, mut cpu, mut mem) = history
                            .filter(|(hk, ..)| *hk == k)
                            .unwrap_or((k, Ring::default(), Ring::default()));
                        cpu.push(p.cpu_pct.unwrap_or(0.0));
                        mem.push(mib(p.raw.private_ws));
                        Some((k, cpu, mem))
                    });
                let (mut cpu_history, mut mem_history) = prev
                    .as_ref()
                    .map(|f| (f.cpu_history, f.mem_history))
                    .unwrap_or_default();
                cpu_history.push(snapshot.total_cpu_pct().unwrap_or(0.0));
                mem_history.push(mem_pct(stats.used_ram, stats.total_ram));
                let frame = Arc::new(Frame {
                    snapshot,
                    statics: Arc::clone(&statics),
                    cpu_history,
                    mem_history,
                    tick_ms: started.elapsed().as_secs_f32() * 1000.0,
                    scan_ms,
                    env: env.clone(),
                    sockets,
                    context: Arc::clone(&context),
                    history,
                    gpu,
                });
                send_latest(frame_tx, drain, Arc::clone(&frame));
                notify();
                prev = Some(frame);
            }
            Err(e) => tracing::warn!("scan failed: {e}"),
        }
        // Wait for the tick, re-reading the interest as it changes.
        loop {
            let deadline = started + tick_for(&interest);
            let Some(left) = deadline.checked_duration_since(Instant::now()) else {
                break;
            };
            match interest_rx.recv_timeout(left) {
                Ok(next) => {
                    env_stale |= next.refresh_now;
                    interest = Interest {
                        refresh_now: false,
                        ..next
                    };
                    if next.refresh_now {
                        break;
                    }
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc};
    use std::time::Instant;

    fn focused() -> Interest {
        Interest::default()
    }

    #[test]
    fn tick_for_visibility() {
        let mut i = focused();
        assert_eq!(tick_for(&i), Duration::from_secs(1));
        i.visibility = Visibility::Visible;
        assert_eq!(tick_for(&i), Duration::from_secs(2));
        i.visibility = Visibility::Minimized;
        assert_eq!(tick_for(&i), Duration::from_secs(5));
    }

    #[test]
    fn tick_override_clamped() {
        let mut i = focused();
        i.tick_override = Some(Duration::from_millis(100));
        assert_eq!(tick_for(&i), MIN_TICK);
        i.tick_override = Some(Duration::from_secs(60));
        assert_eq!(tick_for(&i), MAX_TICK);
        i.tick_override = Some(Duration::from_secs(3));
        assert_eq!(tick_for(&i), Duration::from_secs(3));
    }

    #[test]
    fn minimized_wins_over_a_fast_override() {
        let i = Interest {
            visibility: Visibility::Minimized,
            tick_override: Some(Duration::from_secs(1)),
            ..focused()
        };
        assert_eq!(tick_for(&i), Duration::from_secs(5));
    }

    #[test]
    fn send_latest_drops_oldest() {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let drain = rx.clone();
        send_latest(&tx, &drain, 1);
        send_latest(&tx, &drain, 2);
        assert_eq!(rx.try_recv(), Ok(2));
        assert!(rx.try_recv().is_err());
    }

    fn fake() -> Box<dyn Platform> {
        let k = |pid, t| ProcKey {
            pid,
            create_time: t,
        };
        let mut f = FakePlatform::new(4);
        let mut a = RawProc::new(k(10, 1), 0, "a.exe");
        f.push_frame(vec![a.clone()]);
        a.cpu_100ns = 5_000_000;
        f.push_frame(vec![a]);
        Box::new(f)
    }

    #[test]
    #[allow(clippy::float_cmp)]
    fn collector_emits_frames_with_cpu_from_the_second_tick() {
        let c = spawn(fake(), Box::new(|| {})).unwrap();
        c.interest
            .send(Interest {
                tick_override: Some(MIN_TICK),
                ..focused()
            })
            .unwrap();
        let f1 = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let mut f2 = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        while f2.snapshot.procs[0].cpu_pct.is_none() {
            f2 = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        }
        assert_eq!(f1.snapshot.procs[0].cpu_pct, None);
        assert_eq!(f2.snapshot.procs[0].cpu_pct, Some(12.5));
        assert!(!f2.cpu_history.is_empty());
        assert!(f2.scan_ms >= 0.0 && f2.scan_ms <= f2.tick_ms);
        assert!(f2.statics.contains_key(&f2.snapshot.procs[0].raw.key));
    }

    #[test]
    fn collector_stops_when_the_ui_side_is_dropped() {
        let c = spawn(fake(), Box::new(|| {})).unwrap();
        let _ = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let Collector {
            interest,
            frames,
            thread,
        } = c;
        drop(interest);
        drop(frames);
        let t = Instant::now();
        thread.join().unwrap();
        assert!(t.elapsed() < Duration::from_secs(3));
    }

    #[test]
    fn selected_process_env_is_read_once_per_selection() {
        let key = ProcKey {
            pid: 10,
            create_time: 1,
        };
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(key, 0, "a.exe")]);
        f.set_env(
            key,
            ProcEnv {
                cwd: Arc::from(r"C:\src\shop\"),
                vars: vec![("PORT".into(), "3000".into())],
            },
        );
        let c = spawn(Box::new(f), Box::new(|| {})).unwrap();
        let first = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.env.is_none());
        c.interest
            .send(Interest {
                selected: Some(key),
                refresh_now: true,
                ..focused()
            })
            .unwrap();
        let f2 = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let (k2, env) = f2.env.clone().unwrap();
        assert_eq!(k2, key);
        assert_eq!(env.ok().map(|e| e.vars.len()), Some(1));
        // Same selection on later ticks reuses the same read.
        c.interest
            .send(Interest {
                selected: Some(key),
                ..focused()
            })
            .unwrap();
        c.interest
            .send(Interest {
                selected: Some(key),
                refresh_now: false,
                tick_override: Some(MIN_TICK),
                ..focused()
            })
            .unwrap();
        let f3 = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(Arc::ptr_eq(&f3.env.clone().unwrap().1, &env));
    }

    #[test]
    fn sockets_are_read_only_when_wanted() {
        use crate::ports::Proto;
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(
            ProcKey {
                pid: 10,
                create_time: 1,
            },
            0,
            "node.exe",
        )]);
        f.sockets = vec![Socket {
            proto: Proto::Tcp,
            local: "127.0.0.1:3000".parse().unwrap(),
            remote: None,
            state: None,
            owner: 10,
        }];
        let c = spawn(Box::new(f), Box::new(|| {})).unwrap();
        let first = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.sockets.is_none());
        c.interest
            .send(Interest {
                want_ports: true,
                refresh_now: true,
                ..focused()
            })
            .unwrap();
        let next = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(next.sockets.as_ref().map(|s| s.len()), Some(1));
    }

    fn gpu_fake(gpu: Option<Vec<crate::gpu::GpuSample>>) -> Box<FakePlatform> {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(
            ProcKey {
                pid: 10,
                create_time: 1,
            },
            0,
            "game.exe",
        )]);
        f.gpu = gpu;
        Box::new(f)
    }

    fn want_gpu(c: &Collector, on: bool) -> Arc<Frame> {
        c.interest
            .send(Interest {
                want_gpu: on,
                refresh_now: true,
                ..focused()
            })
            .unwrap();
        c.frames.recv_timeout(Duration::from_secs(5)).unwrap()
    }

    #[test]
    fn gpu_is_sampled_only_while_wanted() {
        let sample = crate::gpu::GpuSample {
            pid: 10,
            util_pct: 40.0,
            dedicated: 1 << 20,
        };
        let c = spawn(gpu_fake(Some(vec![sample])), Box::new(|| {})).unwrap();
        let first = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(first.gpu.is_none());
        let on = want_gpu(&c, true);
        let key = ProcKey {
            pid: 10,
            create_time: 1,
        };
        assert_eq!(
            on.gpu
                .as_ref()
                .and_then(|g| g.get(&key))
                .map(|u| u.util_pct),
            Some(40.0)
        );
        assert!(want_gpu(&c, false).gpu.is_none());
    }

    #[test]
    fn a_machine_without_gpu_counters_gives_no_gpu_figures() {
        let c = spawn(gpu_fake(None), Box::new(|| {})).unwrap();
        let _ = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let f = want_gpu(&c, true);
        assert!(f.gpu.is_none());
        assert_eq!(f.snapshot.procs.len(), 1, "the scan still works");
    }

    #[test]
    fn refresh_now_scans_without_waiting_for_the_tick() {
        let c = spawn(fake(), Box::new(|| {})).unwrap();
        let _ = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        let t = Instant::now();
        c.interest
            .send(Interest {
                refresh_now: true,
                ..focused()
            })
            .unwrap();
        let _ = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(t.elapsed() < Duration::from_millis(500));
    }

    #[test]
    fn frames_carry_labels_and_the_selected_process_history() {
        let key = |pid| ProcKey {
            pid,
            create_time: 1,
        };
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(key(10), 0, "wslhost.exe"),
            RawProc::new(key(11), 0, "a.exe"),
        ]);
        let c = spawn(Box::new(f), Box::new(|| {})).unwrap();
        let first = c.frames.recv_timeout(Duration::from_secs(5)).unwrap();
        assert_eq!(
            first.context.get(&key(10)).and_then(|c| c.label.as_deref()),
            Some("WSL host")
        );
        assert!(!first.context.contains_key(&key(11)));
        assert!(first.history.is_none());
        let select = |k| {
            c.interest
                .send(Interest {
                    selected: Some(k),
                    refresh_now: true,
                    ..focused()
                })
                .unwrap();
            c.frames.recv_timeout(Duration::from_secs(5)).unwrap()
        };
        let a = select(key(10));
        let b = select(key(10));
        let (ka, cpu_a, _) = a.history.unwrap();
        let (_, cpu_b, mem_b) = b.history.unwrap();
        assert_eq!(ka, key(10));
        assert_eq!(cpu_b.len(), cpu_a.len() + 1);
        assert_eq!(mem_b.len(), cpu_b.len());
        // A new selection starts a new history.
        let (kc, cpu_c, _) = select(key(11)).history.unwrap();
        assert_eq!(kc, key(11));
        assert_eq!(cpu_c.len(), 1);
    }
}
