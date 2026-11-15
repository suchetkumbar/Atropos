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
use super::respawn::{self, Respawn};
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
    /// End every process started from the target's terminal window (spec §4.6).
    KillConsole {
        key: ProcKey,
        opts: KillOpts,
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
            | Request::Check { key, .. }
            | Request::KillConsole { key, .. } => key,
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
        /// For kills: what would start the target again (spec Phase 4 respawn warnings).
        respawn: Option<Respawn>,
    },
}

fn fresh(platform: &mut dyn Platform) -> Snapshot {
    let mut raw = Vec::new();
    match platform.scan(&mut raw) {
        Ok(stats) => build(None, raw, stats),
        Err(_) => Snapshot::default(),
    }
}

/// Ends every process on `key`'s console except its ancestors and Atropos (spec §4.6).
fn kill_console(
    platform: &mut dyn Platform,
    snap: &Snapshot,
    key: ProcKey,
    me: ProcKey,
    opts: KillOpts,
    blocked: &dyn Fn(ProcKey) -> bool,
    emit: &mut dyn FnMut(Event),
) -> TreeReport {
    let pids = platform
        .console_processes(key)
        .unwrap_or_else(|_| vec![key.pid]);
    let roots: Vec<ProcKey> = super::subtree::console_roots(snap, key, &pids)
        .into_iter()
        .filter(|k| *k != me)
        .collect();
    let mut all = TreeReport::default();
    for root in roots {
        let r = kill_tree(platform, root, opts, blocked, &mut |p| {
            emit(Event::Progress(key, p));
        });
        for m in r.members {
            if !all.members.iter().any(|(k, _)| *k == m.0) {
                all.members.push(m);
            }
        }
    }
    all
}

/// What would restart `key` after a kill. The supervisor is recognised by its command line, so
/// the ancestors' statics are read here.
fn respawn_for(
    platform: &mut dyn Platform,
    snap: &Snapshot,
    statics: &StaticsMap,
    services: &[crate::ports::ServiceInfo],
    key: ProcKey,
) -> Option<Respawn> {
    let mut all = statics.clone();
    let mut up = snap.get(key).and_then(|p| p.parent);
    while let Some(i) = up {
        let k = snap.procs[i].raw.key;
        all.entry(k)
            .or_insert_with(|| Arc::new(platform.statics(k)));
        up = snap.procs[i].parent;
    }
    respawn::check(snap, &all, services, key)
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
        Request::KillConsole { .. } => (ActionKind::KillTree, true),
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
        Request::KillConsole { opts, .. } => {
            let report = kill_console(platform, &snap, key, me, opts, &blocked, emit);
            emit(Event::TreeKilled(key, report));
        }
        Request::Check { kind, .. } => {
            let verdict = policy::check(&ctx, key, kind);
            let respawn = matches!(kind, ActionKind::Kill | ActionKind::KillTree)
                .then(|| respawn_for(platform, &snap, &statics, &services, key))
                .flatten();
            emit(Event::Checked {
                key,
                kind,
                verdict,
                respawn,
            });
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
    fn console_scope_ends_the_window_but_not_the_shell_or_atropos() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(1), 0, "WindowsTerminal.exe"),
            RawProc::new(k(5), 1, "pwsh.exe"),
            RawProc::new(k(10), 5, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(12), 1, "node.exe"),
            RawProc::new(ME, 1, "Atropos.exe"),
        ]);
        f.set_console(k(10), vec![5, 10, 11, 12, 3]);
        let ev = run(
            &mut f,
            Request::KillConsole {
                key: k(10),
                opts: KillOpts::default(),
            },
        );
        let Some(Event::TreeKilled(key, report)) = ev.last() else {
            panic!("{ev:?}");
        };
        assert_eq!(*key, k(10));
        let ended: Vec<u32> = report.members.iter().map(|(k, _)| k.pid).collect();
        assert_eq!(ended, [10, 11, 12]);
        assert!(
            report
                .members
                .iter()
                .all(|(_, o)| *o == Outcome::Terminated)
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
            [Event::Checked { key, kind: ActionKind::Kill, verdict: Verdict::Hard(_), .. }] if *key == k(20)
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
                verdict: Verdict::Allowed,
                respawn: None,
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

    #[test]
    fn check_reports_what_would_respawn_the_target() {
        use crate::statics::Statics;
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(ME, 0, "Atropos.exe"),
            RawProc::new(k(10), 0, "node.exe"),
            RawProc::new(k(11), 10, "cmd.exe"),
            RawProc::new(k(12), 11, "node.exe"),
        ]);
        f.set_statics(
            k(10),
            Statics {
                cmdline: crate::Field::Ok(Arc::from(
                    "node x/node_modules/nodemon/bin/nodemon.js app.js",
                )),
                ..Statics::unavailable(&crate::Field::Unsupported)
            },
        );
        let check =
            |f: &mut FakePlatform, key, kind| match run(f, Request::Check { key, kind }).pop() {
                Some(Event::Checked { respawn, .. }) => respawn,
                other => panic!("{other:?}"),
            };
        assert_eq!(
            check(&mut f, k(12), ActionKind::Kill),
            Some(Respawn::Supervisor {
                key: k(10),
                name: "nodemon"
            })
        );
        assert_eq!(check(&mut f, k(10), ActionKind::KillTree), None);
        assert_eq!(check(&mut f, k(12), ActionKind::Suspend), None);
    }
}
