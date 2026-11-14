//! The executor: runs actions on its own thread with its own `Platform`, so a slow kill never
//! blocks the UI or the collector. Requests in, events (progress + one final event) out.
//!
//! The policy is re-checked here against a fresh scan: hard blocks are refused (`Protected`)
//! whatever the UI showed. Soft blocks are the UI's job (typed confirmation) before sending.

use std::collections::HashMap;
use std::sync::Arc;
use std::thread::JoinHandle;

use crossbeam_channel::{Receiver, Sender};

use super::kill::{KillOpts, Outcome, Progress, kill};
use super::policy::{self, ActionKind, Context, Verdict};
use super::subtree::{TreeReport, kill_tree};
use super::suspend::{SuspendLedger, Toggle};
use crate::ProcKey;
use crate::platform::Platform;
use crate::snapshot::{Snapshot, build};
use crate::statics::StaticsMap;

/// What the UI asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Request {
    Kill {
        key: ProcKey,
        tree: bool,
        opts: KillOpts,
    },
    Suspend {
        key: ProcKey,
        tree: bool,
    },
    Resume {
        key: ProcKey,
        tree: bool,
    },
    /// Policy pre-check for the confirmation dialog (answers with `Event::Checked`).
    Check {
        key: ProcKey,
        kind: ActionKind,
    },
}

impl Request {
    #[must_use]
    pub fn key(&self) -> ProcKey {
        match *self {
            Request::Kill { key, .. }
            | Request::Suspend { key, .. }
            | Request::Resume { key, .. }
            | Request::Check { key, .. } => key,
        }
    }
}

/// What the executor reports. Each request ends with exactly one of the last three.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Progress(ProcKey, Progress),
    Killed(ProcKey, Outcome),
    TreeKilled(ProcKey, TreeReport),
    /// `suspend` = true for suspend, false for resume; per member, plus Atropos's outstanding
    /// suspend count for each.
    Toggled {
        key: ProcKey,
        suspend: bool,
        results: Vec<(ProcKey, Toggle, u32)>,
    },
    /// The policy verdict for a `Request::Check`.
    Checked {
        key: ProcKey,
        kind: ActionKind,
        verdict: Verdict,
    },
}

fn fresh(platform: &mut dyn Platform) -> Snapshot {
    let mut raw = Vec::new();
    match platform.scan(&mut raw) {
        Ok(stats) => build(None, raw, stats),
        Err(_) => Snapshot::default(),
    }
}

/// Runs one request to completion, emitting progress and exactly one final event.
pub fn handle(
    platform: &mut dyn Platform,
    ledger: &mut SuspendLedger,
    me: ProcKey,
    req: Request,
    emit: &mut dyn FnMut(Event),
) {
    let snap = fresh(platform);
    let services = platform.services().unwrap_or_default();
    let key = req.key();
    let (kind, tree) = match req {
        Request::Kill { tree, .. } => (
            if tree {
                ActionKind::KillTree
            } else {
                ActionKind::Kill
            },
            tree,
        ),
        Request::Suspend { tree, .. } => (ActionKind::Suspend, tree),
        Request::Resume { tree, .. } => (ActionKind::Resume, tree),
        Request::Check { kind, .. } => (kind, kind == ActionKind::KillTree),
    };
    // Protection status needs statics; read them for the target (and its subtree) only.
    let keys: Vec<ProcKey> = match snap.index.get(&key) {
        Some(&i) if tree => super::subtree::preorder(&snap, i),
        _ => vec![key],
    };
    let mut statics = StaticsMap::new();
    for k in keys.iter().filter(|k| snap.index.contains_key(k)) {
        statics.insert(*k, Arc::new(platform.statics(*k)));
    }
    let ctx = Context {
        snapshot: &snap,
        statics: &statics,
        services: &services,
        me,
    };
    let hard: HashMap<ProcKey, bool> = keys
        .iter()
        .map(|k| {
            (
                *k,
                matches!(policy::check(&ctx, *k, kind), Verdict::Hard(_)),
            )
        })
        .collect();
    // Members found later (settle rounds) are checked on the spot.
    let blocked = |k: ProcKey| {
        hard.get(&k)
            .copied()
            .unwrap_or_else(|| matches!(policy::check(&ctx, k, kind), Verdict::Hard(_)))
    };
    match req {
        Request::Check { kind, .. } => {
            let verdict = policy::check(&ctx, key, kind);
            emit(Event::Checked { key, kind, verdict });
        }
        Request::Kill {
            opts, tree: false, ..
        } => {
            let o = kill(platform, key, opts, blocked(key), &mut |p| {
                emit(Event::Progress(key, p));
            });
            emit(Event::Killed(key, o));
        }
        Request::Kill {
            opts, tree: true, ..
        } => {
            let r = kill_tree(platform, key, opts, &blocked, &mut |p| {
                emit(Event::Progress(key, p));
            });
            emit(Event::TreeKilled(key, r));
        }
        Request::Suspend { .. } | Request::Resume { .. } => {
            let suspend = matches!(req, Request::Suspend { .. });
            let res = if suspend {
                ledger.suspend(platform, &snap, key, tree, &blocked)
            } else {
                ledger.resume(platform, &snap, key, tree, &blocked)
            };
            let results = res
                .into_iter()
                .map(|(k, t)| (k, t, ledger.count(k)))
                .collect();
            emit(Event::Toggled {
                key,
                suspend,
                results,
            });
        }
    }
}

/// A running executor thread.
pub struct Executor {
    pub requests: Sender<Request>,
    pub events: Receiver<Event>,
    pub thread: JoinHandle<()>,
}

/// Starts the executor on `platform`; `notify` runs after every event (the GUI repaints).
///
/// # Errors
/// If the OS refuses to start a thread.
pub fn spawn(
    mut platform: Box<dyn Platform>,
    me: ProcKey,
    notify: Box<dyn Fn() + Send>,
) -> std::io::Result<Executor> {
    let (req_tx, req_rx) = crossbeam_channel::unbounded::<Request>();
    let (ev_tx, ev_rx) = crossbeam_channel::unbounded();
    let thread = std::thread::Builder::new()
        .name("atropos-executor".into())
        .spawn(move || {
            let mut ledger = SuspendLedger::default();
            for req in req_rx {
                handle(platform.as_mut(), &mut ledger, me, req, &mut |e| {
                    let _ = ev_tx.send(e);
                    notify();
                });
            }
        })?;
    Ok(Executor {
        requests: req_tx,
        events: ev_rx,
        thread,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::kill::Strategy;
    use crate::platform::{FakePlatform, RawProc, Reaction};
    use std::time::Duration;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    const ME: ProcKey = ProcKey {
        pid: 3,
        create_time: 3,
    };

    fn fake() -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(2), 1, "WindowsTerminal.exe"),
            RawProc::new(ME, 2, "Atropos.exe"),
            RawProc::new(k(10), 1, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(20), 1, "lsass.exe"),
        ]);
        f
    }

    fn run(f: &mut FakePlatform, req: Request) -> Vec<Event> {
        let mut ledger = SuspendLedger::default();
        let mut out = Vec::new();
        handle(f, &mut ledger, ME, req, &mut |e| out.push(e));
        out
    }

    fn kill_req(key: ProcKey, tree: bool) -> Request {
        Request::Kill {
            key,
            tree,
            opts: KillOpts::default(),
        }
    }

    #[test]
    fn kill_reports_progress_then_one_final_event() {
        let mut f = fake();
        f.set_strategy(k(10), Strategy::Window);
        f.set_reaction(k(10), Reaction::HonoursGraceful);
        let ev = run(&mut f, kill_req(k(10), false));
        assert!(
            matches!(ev.first(), Some(Event::Progress(key, Progress::Closing { .. })) if *key == k(10))
        );
        assert_eq!(ev.last(), Some(&Event::Killed(k(10), Outcome::Exited)));
        let finals = ev
            .iter()
            .filter(|e| !matches!(e, Event::Progress(..)))
            .count();
        assert_eq!(finals, 1);
    }

    #[test]
    fn hard_blocked_target_is_protected_whatever_the_ui_sent() {
        let mut f = fake();
        assert_eq!(
            run(&mut f, kill_req(k(20), false)),
            vec![Event::Killed(k(20), Outcome::Protected)]
        );
        // Atropos's own ancestor (the terminal) too.
        assert_eq!(
            run(&mut f, kill_req(k(2), true)).last(),
            Some(&Event::TreeKilled(
                k(2),
                TreeReport {
                    members: vec![(k(2), Outcome::Protected)]
                }
            ))
        );
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn tree_kill_reports_every_member() {
        let mut f = fake();
        let ev = run(&mut f, kill_req(k(10), true));
        let Some(Event::TreeKilled(key, report)) = ev.last() else {
            panic!("{ev:?}");
        };
        assert_eq!(*key, k(10));
        assert_eq!(
            report.members,
            vec![(k(10), Outcome::Terminated), (k(11), Outcome::Terminated)]
        );
    }

    #[test]
    fn suspend_and_resume_report_counts() {
        let mut f = fake();
        let mut ledger = SuspendLedger::default();
        let mut out = Vec::new();
        handle(
            &mut f,
            &mut ledger,
            ME,
            Request::Suspend {
                key: k(10),
                tree: false,
            },
            &mut |e| out.push(e),
        );
        handle(
            &mut f,
            &mut ledger,
            ME,
            Request::Suspend {
                key: k(10),
                tree: false,
            },
            &mut |e| out.push(e),
        );
        assert_eq!(
            out.last(),
            Some(&Event::Toggled {
                key: k(10),
                suspend: true,
                results: vec![(k(10), Toggle::Done, 2)]
            })
        );
        handle(
            &mut f,
            &mut ledger,
            ME,
            Request::Resume {
                key: k(10),
                tree: false,
            },
            &mut |e| out.push(e),
        );
        assert_eq!(
            out.last(),
            Some(&Event::Toggled {
                key: k(10),
                suspend: false,
                results: vec![(k(10), Toggle::Done, 1)]
            })
        );
    }

    #[test]
    fn check_returns_the_policy_verdict_without_acting() {
        let mut f = fake();
        let ev = run(
            &mut f,
            Request::Check {
                key: k(20),
                kind: ActionKind::Kill,
            },
        );
        assert!(matches!(
            ev.as_slice(),
            [Event::Checked { key, kind: ActionKind::Kill, verdict: Verdict::Hard(_) }] if *key == k(20)
        ));
        let ev = run(
            &mut f,
            Request::Check {
                key: k(10),
                kind: ActionKind::KillTree,
            },
        );
        assert_eq!(
            ev,
            vec![Event::Checked {
                key: k(10),
                kind: ActionKind::KillTree,
                verdict: Verdict::Allowed
            }]
        );
        assert_eq!(
            f.terminate_calls() + f.graceful_calls() + f.suspend_log().len(),
            0
        );
    }

    #[test]
    fn spawned_executor_runs_requests_and_stops_when_dropped() {
        let ex = spawn(Box::new(fake()), ME, Box::new(|| {})).unwrap();
        ex.requests.send(kill_req(k(10), false)).unwrap();
        let mut last = None;
        while let Ok(e) = ex.events.recv_timeout(Duration::from_secs(10)) {
            let done = !matches!(e, Event::Progress(..));
            last = Some(e);
            if done {
                break;
            }
        }
        assert_eq!(last, Some(Event::Killed(k(10), Outcome::Terminated)));
        let Executor {
            requests, thread, ..
        } = ex;
        drop(requests);
        thread.join().unwrap();
    }
}
