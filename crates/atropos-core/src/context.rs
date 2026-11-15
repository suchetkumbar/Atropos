//! Per-process context (spec §4.8, Phase 4): the smart label, the working directory and the
//! repository it belongs to. Resolved once per [`ProcKey`] on the collector thread, with two
//! limits so the tick stays cheap: only a few working directories are read per tick, and the
//! file system is probed on a separate thread ([`AsyncDirs`]) — a label that needs the directory
//! simply appears a tick later.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use crossbeam_channel::{Receiver, Sender};

use crate::labels::{Dir, DirInfo, Labels, Resolution, project};
use crate::platform::Platform;
use crate::snapshot::{Proc, Snapshot};
use crate::statics::StaticsMap;
use crate::{Field, ProcKey};

/// What is known about one process beyond its statics. Absent fields are simply unknown.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Ctx {
    pub label: Option<Arc<str>>,
    pub cwd: Option<Arc<str>>,
    /// Root of the git repository the cwd is in.
    pub repo: Option<Arc<str>>,
}

/// Context by key; processes with nothing known are absent.
pub type ContextMap = HashMap<ProcKey, Ctx>;

/// Looks up directory facts. `None` means "not known yet, ask again next tick".
pub trait DirLookup {
    fn get(&mut self, dir: &str) -> Option<Arc<DirInfo>>;
}

/// Probes on the calling thread (the CLI and tests).
#[derive(Debug, Default)]
pub struct SyncDirs {
    markers: Vec<String>,
    cache: HashMap<String, Arc<DirInfo>>,
}

impl SyncDirs {
    #[must_use]
    pub fn new(markers: Vec<String>) -> Self {
        Self {
            markers,
            cache: HashMap::new(),
        }
    }

    /// Pre-seeds an answer (tests).
    pub fn insert(&mut self, dir: &str, info: DirInfo) {
        self.cache.insert(dir.to_owned(), Arc::new(info));
    }
}

impl DirLookup for SyncDirs {
    fn get(&mut self, dir: &str) -> Option<Arc<DirInfo>> {
        if let Some(info) = self.cache.get(dir) {
            return Some(Arc::clone(info));
        }
        let info = Arc::new(project::probe(std::path::Path::new(dir), &self.markers));
        self.cache.insert(dir.to_owned(), Arc::clone(&info));
        Some(info)
    }
}

/// Directories remembered before the cache is started afresh.
const MAX_DIRS: usize = 4096;

/// Probes on its own thread, so a slow disk or a dead network share never delays a tick.
#[derive(Debug)]
pub struct AsyncDirs {
    tx: Sender<String>,
    rx: Receiver<(String, DirInfo)>,
    /// `None` = asked, no answer yet.
    cache: HashMap<String, Option<Arc<DirInfo>>>,
}

impl AsyncDirs {
    /// Starts the prober thread; it stops when this value is dropped.
    ///
    /// # Errors
    /// If the OS refuses to start a thread.
    pub fn spawn(markers: Vec<String>) -> std::io::Result<Self> {
        let (tx, work) = crossbeam_channel::unbounded::<String>();
        let (done, rx) = crossbeam_channel::unbounded();
        std::thread::Builder::new()
            .name("atropos-dirs".into())
            .spawn(move || {
                for dir in work {
                    let info = project::probe(std::path::Path::new(&dir), &markers);
                    if done.send((dir, info)).is_err() {
                        return;
                    }
                }
            })?;
        Ok(Self {
            tx,
            rx,
            cache: HashMap::new(),
        })
    }
}

impl DirLookup for AsyncDirs {
    fn get(&mut self, dir: &str) -> Option<Arc<DirInfo>> {
        while let Ok((d, info)) = self.rx.try_recv() {
            self.cache.insert(d, Some(Arc::new(info)));
        }
        if let Some(known) = self.cache.get(dir) {
            return known.clone();
        }
        if self.cache.len() >= MAX_DIRS {
            self.cache.retain(|_, v| v.is_none());
        }
        if self.tx.send(dir.to_owned()).is_ok() {
            self.cache.insert(dir.to_owned(), None);
        }
        None
    }
}

#[derive(Debug, Default)]
struct Entry {
    /// `None` = not read yet.
    cwd: Option<Field<Arc<str>>>,
    /// `None` = not resolved yet; `Some(None)` = resolved: no rule matches.
    #[allow(clippy::option_option)] // exactly these three states
    label: Option<Option<Arc<str>>>,
    /// Nothing more to learn unless the cwd becomes wanted.
    settled: bool,
}

/// Resolves and remembers [`Ctx`] for exactly the keys in the latest snapshot.
#[derive(Debug)]
pub struct ContextCache {
    labels: Labels,
    entries: HashMap<ProcKey, Entry>,
    map: ContextMap,
    /// Most working directories read in one tick.
    pub reads_per_tick: usize,
    /// Stop reading working directories once a tick has spent this long on them.
    pub time_per_tick: Duration,
}

impl ContextCache {
    #[must_use]
    pub fn new(labels: Labels) -> Self {
        Self {
            labels,
            entries: HashMap::new(),
            map: ContextMap::new(),
            reads_per_tick: 64,
            time_per_tick: Duration::from_millis(2),
        }
    }

    /// Every marker the rules ask for (for the directory prober).
    #[must_use]
    pub fn markers(&self) -> Vec<String> {
        self.labels.markers()
    }

    #[must_use]
    pub fn map(&self) -> &ContextMap {
        &self.map
    }

    /// Updates the context for `snap`. The cwd is read for processes whose label rule needs it and
    /// for those `want_cwd` selects (project grouping, leftovers). Returns whether the map changed.
    pub fn refresh(
        &mut self,
        platform: &mut dyn Platform,
        snap: &Snapshot,
        statics: &StaticsMap,
        dirs: &mut dyn DirLookup,
        want_cwd: &dyn Fn(&Proc) -> bool,
    ) -> bool {
        let before = self.map.len();
        self.entries.retain(|k, _| snap.index.contains_key(k));
        self.map.retain(|k, _| snap.index.contains_key(k));
        let mut changed = self.map.len() != before;
        let started = Instant::now();
        let mut reads = 0usize;
        for p in &snap.procs {
            let key = p.raw.key;
            let e = self.entries.entry(key).or_default();
            let wanted = want_cwd(p);
            if e.settled && (!wanted || e.cwd.is_some()) {
                continue;
            }
            let cmdline = statics.get(&key).and_then(|s| s.cmdline.ok()).map(|c| &**c);
            let mut need_cwd = wanted;
            if e.label.is_none() && e.cwd.is_none() {
                match self.labels.resolve(&p.raw.image, cmdline, Dir::Unknown) {
                    Resolution::Label(l) => e.label = Some(l.map(Arc::from)),
                    Resolution::NeedDir => need_cwd = true,
                }
            }
            if need_cwd
                && e.cwd.is_none()
                && reads < self.reads_per_tick
                && (reads == 0 || started.elapsed() < self.time_per_tick)
            {
                reads += 1;
                e.cwd = Some(match platform.cwd(key) {
                    Field::Ok(c) if c.is_empty() => Field::Unsupported,
                    other => other,
                });
            }
            let cwd = match &e.cwd {
                Some(Field::Ok(c)) => Some(Arc::clone(c)),
                _ => None,
            };
            let info = cwd.as_ref().and_then(|c| dirs.get(c));
            // Still waiting for the cwd read (budget) or for the prober's answer.
            let pending = (need_cwd && e.cwd.is_none()) || (cwd.is_some() && info.is_none());
            if e.label.is_none() && !pending {
                let dir = match &info {
                    Some(i) => Dir::Known(i),
                    None => Dir::Unavailable,
                };
                if let Resolution::Label(l) = self.labels.resolve(&p.raw.image, cmdline, dir) {
                    e.label = Some(l.map(Arc::from));
                }
            }
            e.settled = e.label.is_some() && !pending;
            let ctx = Ctx {
                label: e.label.clone().flatten(),
                repo: info.as_ref().and_then(|i| i.repo.clone()),
                cwd,
            };
            if ctx == Ctx::default() {
                changed |= self.map.remove(&key).is_some();
            } else if self.map.get(&key) != Some(&ctx) {
                self.map.insert(key, ctx);
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, ProcEnv, RawProc};
    use crate::snapshot::build;
    use crate::statics::{Statics, StaticsCache};

    const NEXT: &str = r"node D:\code\shop\node_modules\next\dist\bin\next dev";
    const SHOP: &str = r"D:\code\shop\";

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    fn statics(cmdline: &str) -> Statics {
        Statics {
            cmdline: Field::Ok(Arc::from(cmdline)),
            ..Statics::unavailable(&Field::Unsupported)
        }
    }

    fn shop() -> DirInfo {
        DirInfo {
            markers: vec!["package.json".to_owned()],
            project: Some(Arc::from("shop")),
            repo: Some(Arc::from(r"D:\code\shop")),
        }
    }

    struct World {
        f: FakePlatform,
        statics: StaticsCache,
        cache: ContextCache,
        dirs: SyncDirs,
    }

    impl World {
        /// One frame, replayed forever; `(pid, image, cmdline, cwd)`.
        fn new(procs: &[(u32, &str, &str, Option<&str>)]) -> Self {
            let mut f = FakePlatform::new(1);
            f.push_frame(
                procs
                    .iter()
                    .map(|(pid, image, ..)| RawProc::new(k(*pid), 0, image))
                    .collect(),
            );
            for (pid, _, cmdline, cwd) in procs {
                f.set_statics(k(*pid), statics(cmdline));
                if let Some(cwd) = cwd {
                    f.set_env(
                        k(*pid),
                        ProcEnv {
                            cwd: Arc::from(*cwd),
                            vars: Vec::new(),
                        },
                    );
                }
            }
            let mut dirs = SyncDirs::new(Vec::new());
            dirs.insert(SHOP, shop());
            Self {
                f,
                statics: StaticsCache::new(),
                cache: ContextCache::new(Labels::builtin()),
                dirs,
            }
        }

        fn tick_with(
            &mut self,
            dirs: Option<&mut dyn DirLookup>,
            want: &dyn Fn(&Proc) -> bool,
        ) -> bool {
            let mut out = Vec::new();
            let stats = self.f.scan(&mut out).unwrap();
            let snap = build(None, out, stats);
            self.statics.refresh(&mut self.f, &snap);
            let dirs = match dirs {
                Some(d) => d,
                None => &mut self.dirs,
            };
            self.cache
                .refresh(&mut self.f, &snap, self.statics.map(), dirs, want)
        }

        fn tick(&mut self) -> bool {
            self.tick_with(None, &|_| false)
        }

        fn label(&self, pid: u32) -> Option<&str> {
            self.cache.map().get(&k(pid))?.label.as_deref()
        }
    }

    #[test]
    fn labels_without_a_directory_rule_never_read_the_cwd() {
        let mut w = World::new(&[
            (1, "wslhost.exe", "wslhost.exe", Some(SHOP)),
            (2, "svchost.exe", "svchost.exe -k netsvcs", Some(SHOP)),
        ]);
        assert!(w.tick());
        assert_eq!(w.label(1), Some("WSL host"));
        assert!(!w.cache.map().contains_key(&k(2)));
        assert_eq!(w.f.env_calls(), 0);
    }

    #[test]
    fn a_project_label_reads_the_cwd_once() {
        let mut w = World::new(&[(1, "node.exe", NEXT, Some(SHOP))]);
        assert!(w.tick());
        assert_eq!(w.label(1), Some("Next.js dev · shop"));
        let ctx = &w.cache.map()[&k(1)];
        assert_eq!(ctx.cwd.as_deref(), Some(SHOP));
        assert_eq!(ctx.repo.as_deref(), Some(r"D:\code\shop"));
        assert!(!w.tick(), "nothing changes on the next tick");
        assert_eq!(w.f.env_calls(), 1);
    }

    #[test]
    fn unreadable_cwd_still_gets_the_label_from_the_command_line() {
        let mut w = World::new(&[(1, "node.exe", NEXT, None)]);
        w.tick();
        assert_eq!(w.label(1), Some("Next.js dev"));
        w.tick();
        assert_eq!(w.f.env_calls(), 1, "an unreadable cwd is not retried");
    }

    #[test]
    fn cwd_reads_are_limited_per_tick() {
        let procs: Vec<(u32, &str, &str, Option<&str>)> = (1..=10)
            .map(|pid| (pid, "node.exe", NEXT, Some(SHOP)))
            .collect();
        let mut w = World::new(&procs);
        w.cache.reads_per_tick = 3;
        w.tick();
        assert_eq!(w.f.env_calls(), 3);
        assert_eq!(w.cache.map().len(), 3);
        for _ in 0..3 {
            w.tick();
        }
        assert_eq!(w.f.env_calls(), 10);
        assert!((1..=10).all(|pid| w.label(pid) == Some("Next.js dev · shop")));
    }

    /// Answers "not yet" a number of times, like the prober thread.
    struct Slow {
        pending: u32,
        asked: u32,
    }
    impl DirLookup for Slow {
        fn get(&mut self, _dir: &str) -> Option<Arc<DirInfo>> {
            self.asked += 1;
            if self.pending > 0 {
                self.pending -= 1;
                return None;
            }
            Some(Arc::new(shop()))
        }
    }

    #[test]
    fn a_pending_directory_delays_the_label_without_another_cwd_read() {
        let mut w = World::new(&[(1, "node.exe", NEXT, Some(SHOP))]);
        let mut slow = Slow {
            pending: 2,
            asked: 0,
        };
        w.tick_with(Some(&mut slow), &|_| false);
        assert_eq!(w.label(1), None);
        w.tick_with(Some(&mut slow), &|_| false);
        assert_eq!(w.label(1), None);
        assert!(w.tick_with(Some(&mut slow), &|_| false));
        assert_eq!(w.label(1), Some("Next.js dev · shop"));
        assert_eq!(w.f.env_calls(), 1);
        w.tick_with(Some(&mut slow), &|_| false);
        assert_eq!(slow.asked, 3, "a settled process is not looked up again");
    }

    #[test]
    fn wanted_processes_get_cwd_and_repo_even_without_a_label() {
        let mut w = World::new(&[
            (1, "sleeper.exe", "sleeper.exe", Some(SHOP)),
            (2, "other.exe", "other.exe", Some(SHOP)),
        ]);
        w.tick();
        assert!(w.cache.map().is_empty());
        assert!(w.tick_with(None, &|p| p.raw.key.pid == 1));
        let ctx = &w.cache.map()[&k(1)];
        assert_eq!(ctx.label, None);
        assert_eq!(ctx.repo.as_deref(), Some(r"D:\code\shop"));
        assert!(!w.cache.map().contains_key(&k(2)));
        assert_eq!(w.f.env_calls(), 1);
    }

    #[test]
    fn vanished_keys_are_evicted() {
        let mut w = World::new(&[(1, "wslhost.exe", "wslhost.exe", None)]);
        w.tick();
        assert_eq!(w.cache.map().len(), 1);
        w.f.push_frame(vec![RawProc::new(k(9), 0, "svchost.exe")]);
        // FakePlatform replays its last frame, so two scans reach the new one.
        w.tick();
        w.tick();
        assert!(w.cache.map().is_empty());
        assert!(w.cache.entries.keys().all(|key| *key == k(9)));
    }

    #[test]
    fn async_dirs_answers_on_a_later_call() {
        let dir = std::env::temp_dir().join(format!("atropos-ctx-{}", std::process::id()));
        std::fs::create_dir_all(dir.join(".git")).unwrap();
        std::fs::write(dir.join("package.json"), r#"{"name":"async-shop"}"#).unwrap();
        let mut dirs = AsyncDirs::spawn(vec!["package.json".to_owned()]).unwrap();
        let path = dir.to_str().unwrap();
        let deadline = Instant::now() + Duration::from_secs(10);
        let info = loop {
            if let Some(info) = dirs.get(path) {
                break info;
            }
            assert!(Instant::now() < deadline, "prober never answered");
            std::thread::sleep(Duration::from_millis(5));
        };
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(info.project.as_deref(), Some("async-shop"));
        assert_eq!(info.markers, vec!["package.json".to_owned()]);
        assert_eq!(info.repo.as_deref(), Some(path));
    }
}
