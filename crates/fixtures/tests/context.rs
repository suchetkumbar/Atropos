//! Phase 4 on the real backend: labels with `{project}`, repository detection and the leftover
//! (orphaned dev server) detector, only against listeners this test spawned itself.
#![cfg(windows)]
#![allow(clippy::unwrap_used)] // test code

use std::os::windows::process::CommandExt as _;
use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::context::{ContextCache, ContextMap, SyncDirs};
use atropos_core::engine::kill::{KillOpts, kill};
use atropos_core::labels::{Labels, RuleDef};
use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;
use atropos_core::ports::Socket;
use atropos_core::projects::{groups, leftovers};
use atropos_core::snapshot::{Snapshot, build};
use atropos_core::statics::StaticsCache;
use atropos_fixtures::{SpawnedFixture, fixture_path, spawn_cmd};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// A temp "repository": `.git`, a `package.json` named `lefty`, and a nested `src` folder.
struct Repo(PathBuf);

impl Repo {
    fn new(tag: &str) -> Self {
        let root = std::env::temp_dir().join(format!("atropos répo {tag} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(root.join(".git")).unwrap();
        std::fs::create_dir_all(root.join("src")).unwrap();
        std::fs::write(root.join("package.json"), r#"{"name":"lefty"}"#).unwrap();
        Self(root)
    }
}

impl Drop for Repo {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn rules() -> Labels {
    Labels::with_user(&[RuleDef {
        image: vec!["port-listener".into()],
        markers: vec!["package.json".into()],
        label: "Listener · {project}".into(),
        ..RuleDef::default()
    }])
    .0
}

/// One collector-style pass: scan, statics, sockets, context for `keys`.
fn observe(keys: &[ProcKey]) -> (Snapshot, Vec<Socket>, ContextMap) {
    let mut p = WindowsPlatform::new();
    let mut raw = Vec::new();
    let stats = p.scan(&mut raw).unwrap();
    let snap = build(None, raw, stats);
    let mut statics = StaticsCache::new();
    statics.refresh(&mut p, &snap);
    let sockets = p.sockets().unwrap();
    let mut cache = ContextCache::new(rules());
    // The collector spreads cwd reads over several ticks; one pass here must reach our process.
    cache.reads_per_tick = usize::MAX;
    cache.time_per_tick = Duration::from_secs(30);
    let mut dirs = SyncDirs::new(cache.markers());
    cache.refresh(&mut p, &snap, statics.map(), &mut dirs, &|proc| {
        keys.contains(&proc.raw.key)
    });
    (snap, sockets, cache.map().clone())
}

fn wait_port(f: &SpawnedFixture) {
    let line = f.next_line(Duration::from_secs(5)).unwrap();
    assert!(line.starts_with("port="), "{line}");
}

fn same_dir(a: &str, b: &std::path::Path) -> bool {
    a.trim_end_matches('\\').to_lowercase() == b.to_str().unwrap().to_lowercase()
}

#[test]
fn listener_with_a_living_parent_is_labelled_grouped_and_not_a_leftover() {
    let repo = Repo::new("alive");
    let mut cmd = Command::new(fixture_path("port-listener").unwrap());
    // Started from a sub-folder: the repository is found above the cwd.
    cmd.current_dir(repo.0.join("src"))
        .creation_flags(CREATE_NO_WINDOW);
    let child = spawn_cmd(cmd, &["--secs", "60"]).unwrap();
    wait_port(&child);
    let (snap, sockets, ctx) = observe(&[child.key]);
    let c = ctx.get(&child.key).expect("context for the listener");
    assert!(same_dir(c.repo.as_deref().unwrap(), &repo.0), "{c:?}");
    assert!(same_dir(c.cwd.as_deref().unwrap(), &repo.0.join("src")));
    // `src` has no package.json: the marker rule does not match there.
    assert_eq!(c.label, None);
    // Other real processes may sit in repositories of their own; find ours.
    let all = groups(&snap, &ctx);
    let g = all
        .iter()
        .find(|g| same_dir(&g.repo, &repo.0))
        .expect("a group for the temp repository");
    assert_eq!(g.members, vec![child.key]);
    assert!(g.name.starts_with("atropos répo alive"));
    assert!(
        !leftovers(&snap, &sockets, &ctx).contains(&child.key),
        "its parent (this test) is alive"
    );
}

#[test]
fn listener_whose_parent_exited_is_a_leftover() {
    let repo = Repo::new("orphan");
    // `cmd /c start /b` starts the listener and exits at once: the listener is orphaned.
    let mut cmd = Command::new("cmd.exe");
    cmd.args(["/d", "/c", "start", "", "/b"])
        .arg(fixture_path("port-listener").unwrap())
        .current_dir(&repo.0)
        .creation_flags(CREATE_NO_WINDOW);
    // The key line comes from the listener itself (it inherits the pipe).
    let orphan = spawn_cmd(cmd, &["--secs", "60"]).unwrap();
    wait_port(&orphan);
    assert_ne!(orphan.key.pid, orphan.pid(), "the key is the grandchild's");
    let deadline = Instant::now() + Duration::from_secs(10);
    let (found, ctx) = loop {
        let (snap, sockets, ctx) = observe(&[orphan.key]);
        let found = leftovers(&snap, &sockets, &ctx);
        if found.contains(&orphan.key) || Instant::now() > deadline {
            break (found, ctx);
        }
        std::thread::sleep(Duration::from_millis(100));
    };
    // End our own grandchild through the engine before asserting, so a failure leaves nothing.
    let mut p = WindowsPlatform::new();
    let outcome = kill(
        &mut p,
        orphan.key,
        KillOpts {
            grace: Duration::from_secs(1),
            force: true,
        },
        false,
        &mut |_| {},
    );
    assert!(
        found.contains(&orphan.key),
        "not detected; killed: {outcome:?}"
    );
    let c = &ctx[&orphan.key];
    assert_eq!(c.label.as_deref(), Some("Listener · lefty"));
    assert!(same_dir(c.repo.as_deref().unwrap(), &repo.0));
}

/// Manual QA aid (read-only): prints how the built-in rules label this machine's processes.
/// `cargo test -p atropos-fixtures --test context -- --ignored --nocapture live_labels`
#[test]
#[ignore = "prints the live machine's labels; read-only"]
fn live_labels() {
    let mut p = WindowsPlatform::new();
    let mut raw = Vec::new();
    let stats = p.scan(&mut raw).unwrap();
    let snap = build(None, raw, stats);
    let mut statics = StaticsCache::new();
    statics.refresh(&mut p, &snap);
    let mut cache = ContextCache::new(Labels::builtin());
    cache.reads_per_tick = usize::MAX;
    cache.time_per_tick = Duration::from_secs(30);
    let mut dirs = SyncDirs::new(cache.markers());
    let started = Instant::now();
    cache.refresh(&mut p, &snap, statics.map(), &mut dirs, &|_| false);
    let took = started.elapsed();
    let mut counts = std::collections::BTreeMap::<String, usize>::new();
    for proc in &snap.procs {
        if let Some(label) = cache
            .map()
            .get(&proc.raw.key)
            .and_then(|c| c.label.as_deref())
        {
            *counts
                .entry(format!("{label}  [{}]", proc.raw.image))
                .or_default() += 1;
        }
    }
    let labelled: usize = counts.values().sum();
    println!(
        "{labelled} of {} processes labelled in {took:?} (all cwd reads in one pass)",
        snap.procs.len()
    );
    for (label, n) in counts {
        println!("{n:>4}  {label}");
    }
}
