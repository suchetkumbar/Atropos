//! Tick budget with smart labels (Phase 4, M4.2): 10 000 fake processes of which 300 carry a
//! project label keep the collector's own work at p95 ≤ 5 ms, the first (label-resolving) ticks
//! included. Release-only: `cargo test --release -p atropos-core --test label_budget -- --ignored`.

use std::sync::Arc;
use std::time::Instant;

use atropos_core::context::{ContextCache, SyncDirs};
use atropos_core::labels::{DirInfo, Labels};
use atropos_core::platform::{FakePlatform, Platform, ProcEnv, RawProc};
use atropos_core::snapshot::build;
use atropos_core::statics::{Statics, StaticsCache};
use atropos_core::{Field, ProcKey};

#[test]
#[ignore = "timing; run in release"]
fn labelled_processes_tick_under_budget() {
    const CWD: &str = r"D:\code\shop\";
    let key = |i: u32| ProcKey {
        pid: i * 4,
        create_time: u64::from(i),
    };
    let mut f = FakePlatform::new(16);
    let mut procs = Vec::new();
    for i in 1..=10_000u32 {
        let labelled = i <= 300;
        let image = if labelled { "node.exe" } else { "svchost.exe" };
        procs.push(RawProc::new(key(i), (i / 3) * 4, image));
        let cmdline = if labelled {
            format!(r"node D:\code\shop\node_modules\next\dist\bin\next dev --port {i}")
        } else {
            format!("svchost.exe -k netsvcs -p -s Service{i}")
        };
        f.set_statics(
            key(i),
            Statics {
                cmdline: Field::Ok(Arc::from(cmdline)),
                ..Statics::unavailable(&Field::Unsupported)
            },
        );
        if labelled {
            f.set_env(
                key(i),
                ProcEnv {
                    cwd: Arc::from(CWD),
                    vars: Vec::new(),
                },
            );
        }
    }
    f.push_frame(procs);
    let mut statics = StaticsCache::new();
    let mut ctx = ContextCache::new(Labels::builtin());
    let mut dirs = SyncDirs::new(ctx.markers());
    dirs.insert(
        CWD,
        DirInfo {
            markers: vec!["package.json".to_owned()],
            project: Some(Arc::from("shop")),
            repo: None,
        },
    );
    let mut raw = Vec::new();
    let mut prev = None;
    let mut times = Vec::new();
    for _ in 0..40 {
        let t = Instant::now();
        let stats = f.scan(&mut raw).unwrap();
        let snap = build(prev.as_ref(), std::mem::take(&mut raw), stats);
        statics.refresh(&mut f, &snap);
        ctx.refresh(&mut f, &snap, statics.map(), &mut dirs, &|_| false);
        times.push(t.elapsed());
        prev = Some(snap);
    }
    let labelled = ctx
        .map()
        .values()
        .filter(|c| c.label.as_deref() == Some("Next.js dev · shop"))
        .count();
    assert_eq!(labelled, 300);
    assert_eq!(
        ctx.map().len(),
        300,
        "unlabelled processes cost no map entry"
    );
    times.sort();
    let p95 = times[times.len() * 95 / 100];
    println!("p95 = {p95:?}, max = {:?}", times[times.len() - 1]);
    assert!(p95.as_secs_f64() * 1000.0 <= 5.0, "p95 {p95:?}");
}
