//! The action flow in the GUI: shortcut / menu → policy check (executor) → confirmation dialog
//! (or toast for a refusal) → request → live row status → outcome toast.

use std::collections::HashMap;
use std::time::Instant;

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::engine::executor::{self, Event, Executor, Request};
use atropos_core::engine::kill::KillOpts;
use atropos_core::engine::policy::{ActionKind, Verdict};
use atropos_core::platform::windows::{LocalTime, WindowsPlatform};
use eframe::egui;

use crate::dialog::{self, Answer, Dialog, GroupDialog, Kind};
use crate::toast::{self, Retry, Toasts};

/// What the user asked to do to a process.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Act {
    Kill,
    KillTree,
    Suspend,
    Resume,
}

impl Act {
    fn kind(self) -> ActionKind {
        match self {
            Act::Kill => ActionKind::Kill,
            Act::KillTree => ActionKind::KillTree,
            Act::Suspend => ActionKind::Suspend,
            Act::Resume => ActionKind::Resume,
        }
    }
}

/// The elevated retry that fits an executor result: the members Windows refused.
#[must_use]
pub fn retry_for(e: &Event) -> Option<Retry> {
    use atropos_core::engine::kill::Outcome;
    use atropos_core::engine::suspend::Toggle;
    use atropos_helper::protocol::Op;
    let (keys, op): (Vec<ProcKey>, Op) = match e {
        Event::Killed(k, Outcome::Denied) => (vec![*k], Op::GracefulClose),
        Event::TreeKilled(_, r) => (
            r.members
                .iter()
                .filter(|(_, o)| *o == Outcome::Denied)
                .map(|(k, _)| *k)
                .collect(),
            Op::Terminate,
        ),
        Event::Toggled {
            suspend, results, ..
        } => (
            results
                .iter()
                .filter(|(_, t, _)| *t == Toggle::Denied)
                .map(|(k, _, _)| *k)
                .collect(),
            if *suspend { Op::Suspend } else { Op::Resume },
        ),
        _ => return None,
    };
    (!keys.is_empty()).then_some(Retry { keys, op })
}

/// What to do once the policy verdict for `act` is known.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Next {
    /// Ask the user first (kills always; suspends only when soft-blocked).
    Dialog(Kind),
    /// Go ahead without asking.
    Send,
    /// Refused: tell the user why.
    Refuse,
}

/// [`next_step`] under the user's confirmation policy.
#[must_use]
/// A target that would be restarted (`respawns`) always gets the dialog: it carries the warning.
pub fn next_step_with(
    act: Act,
    verdict: &Verdict,
    confirm: atropos_core::config::Confirm,
    respawns: bool,
) -> Next {
    match (act, verdict, confirm) {
        (Act::Kill | Act::KillTree, Verdict::Allowed, atropos_core::config::Confirm::Risky)
            if !respawns =>
        {
            Next::Send
        }
        _ => next_step(act, verdict),
    }
}

/// Decision table: kills always confirm; suspend/resume run directly unless blocked.
#[must_use]
pub fn next_step(act: Act, verdict: &Verdict) -> Next {
    match (act, verdict) {
        (Act::Kill, _) => Next::Dialog(Kind::Kill),
        (Act::KillTree, _) => Next::Dialog(Kind::KillTree),
        (Act::Suspend | Act::Resume, Verdict::Hard(_)) => Next::Refuse,
        (Act::Suspend, Verdict::Soft(_)) => Next::Dialog(Kind::Suspend),
        (Act::Suspend | Act::Resume, _) => Next::Send,
    }
}

/// Action state owned by the app.
pub struct Actions {
    exec: Option<Executor>,
    dialog: Option<Dialog>,
    /// "End everything from `<repo>`" confirmation.
    group: Option<GroupDialog>,
    /// The act a pending check is for.
    pending: HashMap<ProcKey, Act>,
    /// Names captured when the action started (the process may be gone by the end).
    names: HashMap<ProcKey, String>,
    /// Inline row status (`Closing… 3s`).
    status: HashMap<ProcKey, String>,
    /// Atropos's outstanding suspends per process.
    counts: HashMap<ProcKey, u32>,
    pub toasts: Toasts,
    /// Atropos's own key (the helper's parent), when known.
    me: Option<ProcKey>,
    /// Started on the first "Retry as administrator".
    elevated: Option<crate::elevate::Elevated>,
    repaint: Option<egui::Context>,
    /// From the settings.
    pub grace: std::time::Duration,
    pub confirm: atropos_core::config::Confirm,
    journal: Option<std::sync::Arc<std::sync::Mutex<atropos_core::journal::Journal>>>,
}

impl Actions {
    #[must_use]
    pub fn new(exec: Option<Executor>) -> Self {
        Self {
            exec,
            dialog: None,
            group: None,
            pending: HashMap::new(),
            names: HashMap::new(),
            status: HashMap::new(),
            counts: HashMap::new(),
            toasts: Toasts::default(),
            me: None,
            elevated: None,
            repaint: None,
            grace: std::time::Duration::from_secs(5),
            confirm: atropos_core::config::Confirm::Always,
            journal: None,
        }
    }

    /// Starts the executor on the real platform; `atr.exe` next to this exe delivers Ctrl+C.
    #[must_use]
    pub fn spawn(ctx: &egui::Context, me: Option<ProcKey>) -> Self {
        // Without our own identity the policy cannot protect Atropos's ancestors: no actions.
        let Some(me) = me else {
            tracing::error!("own process identity unknown; actions disabled");
            return Self::new(None);
        };
        let mut platform = WindowsPlatform::new();
        if let Some(atr) = std::env::current_exe()
            .ok()
            .map(|exe| exe.with_file_name("atr.exe"))
            .filter(|p| p.is_file())
        {
            platform = platform.with_ctrl_c_helper(atr);
        }
        let repaint = ctx.clone();
        // Every suspend (including the freeze inside a tree kill) is journaled first, so a crash
        // can never leave a process frozen without a record.
        let journal = std::sync::Arc::new(std::sync::Mutex::new(
            atropos_core::journal::Journal::open(atropos_core::journal::path()),
        ));
        let platform = atropos_core::journal::JournaledPlatform::new(
            platform,
            std::sync::Arc::clone(&journal),
        );
        let exec = executor::spawn(
            Box::new(platform),
            me,
            Box::new(move || repaint.request_repaint_after(std::time::Duration::from_millis(1))),
        )
        .inspect_err(|e| tracing::error!("executor thread: {e}"))
        .ok();
        let mut a = Self::new(exec);
        a.journal = Some(journal);
        a.me = Some(me);
        a.repaint = Some(ctx.clone());
        a
    }

    fn send(&mut self, req: Request) {
        if let Some(e) = &self.exec {
            let _ = e.requests.send(req);
        }
    }

    fn name(&self, key: ProcKey) -> String {
        self.names
            .get(&key)
            .cloned()
            .unwrap_or_else(|| "process".to_owned())
    }

    /// Whether a dialog is open (it freezes the row order).
    #[must_use]
    pub fn holds(&self) -> bool {
        self.dialog.is_some() || self.group.is_some()
    }

    /// Opens the confirmation for ending every process of the repository `repo` (spec §4.8).
    /// The plan is made here from the frame; the executor re-checks each tree it is sent.
    pub fn start_group(&mut self, repo: &std::sync::Arc<str>, frame: &Frame) {
        let groups = atropos_core::projects::groups(&frame.snapshot, &frame.context);
        let Some(g) = groups.into_iter().find(|g| g.repo == *repo) else {
            return;
        };
        let ctx = atropos_core::engine::policy::Context {
            snapshot: &frame.snapshot,
            statics: &frame.statics,
            services: &[],
            me: self.me.unwrap_or(ProcKey {
                pid: 0,
                create_time: 0,
            }),
        };
        let plan = atropos_core::projects::plan_group_kill(&ctx, &g.members);
        for key in &plan.roots {
            let name = frame.snapshot.get(*key).map_or_else(
                || "process".to_owned(),
                |p| crate::view::display_name(frame, p).to_owned(),
            );
            self.names.insert(*key, name);
        }
        self.group = Some(GroupDialog {
            name: g.name,
            repo: g.repo,
            plan,
        });
    }

    /// The open group confirmation, if any.
    #[cfg(test)]
    #[must_use]
    pub fn group(&self) -> Option<&GroupDialog> {
        self.group.as_ref()
    }

    /// Answers the group confirmation: ends each planned tree, or cancels.
    pub fn answer_group(&mut self, answer: Answer) {
        let Some(g) = self.group.take() else {
            return;
        };
        if let Answer::Confirm { force } = answer {
            let opts = KillOpts {
                force,
                grace: self.grace,
            };
            for key in g.plan.roots {
                self.status.insert(key, "Starting…".to_owned());
                self.send(Request::Kill {
                    key,
                    tree: true,
                    opts,
                });
            }
        }
    }

    #[must_use]
    pub fn status(&self) -> &HashMap<ProcKey, String> {
        &self.status
    }

    /// Test hook: set a row status directly.
    #[cfg(test)]
    pub fn set_status(&mut self, key: ProcKey, text: &str) {
        self.status.insert(key, text.to_owned());
    }

    #[must_use]
    pub fn suspend_count(&self, key: ProcKey) -> u32 {
        self.counts.get(&key).copied().unwrap_or(0)
    }

    /// Starts `act` on `key` (asks the executor for the policy verdict first).
    pub fn start(&mut self, key: ProcKey, act: Act, frame: &Frame) {
        if self.exec.is_none() {
            self.toasts.push(
                toast::Message {
                    text: "Actions are unavailable (see the log)".to_owned(),
                    level: toast::Level::Error,
                    offer_admin: false,
                    retry: None,
                },
                Instant::now(),
            );
            return;
        }
        let name = frame
            .snapshot
            .get(key)
            .map_or_else(|| "process".to_owned(), |p| p.raw.image.to_string());
        self.names.insert(key, name);
        self.pending.insert(key, act);
        self.send(Request::Check {
            key,
            kind: act.kind(),
        });
    }

    /// Handles executor events.
    pub fn poll(&mut self, now: Instant) {
        self.poll_elevated(now);
        let events: Vec<Event> = match &self.exec {
            Some(e) => e.events.try_iter().collect(),
            None => return,
        };
        for ev in events {
            let retry = retry_for(&ev);
            match ev {
                Event::Progress(key, p) => {
                    self.status.insert(key, toast::progress_text(&p));
                }
                Event::Killed(key, o) => {
                    self.status.remove(&key);
                    let mut m = toast::outcome_message(&self.name(key), key.pid, o);
                    m.retry.clone_from(&retry);
                    self.toasts.push(m, now);
                }
                Event::TreeKilled(key, r) => {
                    self.status.remove(&key);
                    let mut m = toast::tree_message(&self.name(key), key.pid, &r);
                    m.retry.clone_from(&retry);
                    self.toasts.push(m, now);
                }
                Event::Toggled {
                    key,
                    suspend,
                    results,
                } => {
                    self.status.remove(&key);
                    for (k, _, c) in &results {
                        if *c == 0 {
                            self.counts.remove(k);
                        } else {
                            self.counts.insert(*k, *c);
                        }
                    }
                    let mut m = toast::toggle_message(&self.name(key), key, suspend, &results);
                    m.retry.clone_from(&retry);
                    self.toasts.push(m, now);
                }
                Event::Checked {
                    key,
                    kind,
                    verdict,
                    respawn,
                } => {
                    let act = self.pending.remove(&key).unwrap_or(match kind {
                        ActionKind::Kill => Act::Kill,
                        ActionKind::KillTree => Act::KillTree,
                        ActionKind::Suspend => Act::Suspend,
                        ActionKind::Resume => Act::Resume,
                    });
                    match next_step_with(act, &verdict, self.confirm, respawn.is_some()) {
                        Next::Dialog(k) => {
                            self.dialog = Some(Dialog::new(key, k, verdict).with_respawn(respawn));
                        }
                        Next::Send => {
                            let opts = KillOpts {
                                grace: self.grace,
                                force: false,
                            };
                            let req = match act {
                                Act::Resume => Request::Resume { key, tree: false },
                                Act::Suspend => Request::Suspend { key, tree: false },
                                Act::Kill | Act::KillTree => Request::Kill {
                                    key,
                                    tree: act == Act::KillTree,
                                    opts,
                                },
                            };
                            self.send(req);
                        }
                        Next::Refuse => {
                            let why = match &verdict {
                                Verdict::Hard(h) => dialog::hard_text(h),
                                _ => String::new(),
                            };
                            let text = format!("{} ({}): {why}", self.name(key), key.pid);
                            self.toasts.push(
                                toast::Message {
                                    text,
                                    level: toast::Level::Warn,
                                    offer_admin: false,
                                    retry: None,
                                },
                                now,
                            );
                        }
                    }
                }
            }
        }
    }

    /// What a previous run left suspended and is still the same process (PID not reused).
    /// Stale entries are dropped from the journal.
    #[must_use]
    pub fn leftover_suspends(&self) -> Vec<(ProcKey, u32)> {
        let Some(j) = &self.journal else {
            return Vec::new();
        };
        let entries = j.lock().map(|j| j.entries()).unwrap_or_default();
        let live = atropos_core::journal::resumable(&mut WindowsPlatform::new(), &entries);
        if let Ok(mut j) = j.lock() {
            for (k, _) in entries.iter().filter(|e| !live.contains(e)) {
                j.forget(*k);
            }
        }
        live
    }

    /// Resumes leftovers (`resume` = true) or forgets them (left suspended on purpose).
    pub fn settle_leftovers(&mut self, items: &[(ProcKey, u32)], resume: bool) {
        for &(key, n) in items {
            if resume {
                for _ in 0..n {
                    self.send(Request::Resume { key, tree: false });
                }
            } else if let Some(j) = &self.journal {
                if let Ok(mut j) = j.lock() {
                    j.forget(key);
                }
            }
        }
    }

    /// A one-line notice (e.g. a settings problem).
    pub fn notice(&mut self, text: String, now: Instant) {
        self.toast(text, toast::Level::Warn, now);
    }

    fn toast(&mut self, text: String, level: toast::Level, now: Instant) {
        self.toasts.push(
            toast::Message {
                text,
                level,
                offer_admin: false,
                retry: None,
            },
            now,
        );
    }

    /// Results of "Retry as administrator".
    fn poll_elevated(&mut self, now: Instant) {
        use crate::elevate::ElevatedEvent;
        use atropos_helper::protocol::Reply;
        let events: Vec<ElevatedEvent> = match &self.elevated {
            Some(e) => e.events.try_iter().collect(),
            None => return,
        };
        for ev in events {
            match ev {
                ElevatedEvent::Done(key, Reply::Killed(o)) => {
                    let m = toast::outcome_message(&self.name(key), key.pid, o);
                    self.toast(format!("As administrator: {}", m.text), m.level, now);
                }
                ElevatedEvent::Done(key, Reply::Toggled(t)) => {
                    let m = toast::toggle_message(&self.name(key), key, true, &[(key, t, 0)]);
                    let level = m.level;
                    let text = match t {
                        atropos_core::engine::suspend::Toggle::Done => {
                            format!("As administrator: {} ({}) done", self.name(key), key.pid)
                        }
                        _ => format!("As administrator: {}", m.text),
                    };
                    self.toast(text, level, now);
                }
                ElevatedEvent::Done(_, Reply::Rejected(r)) => {
                    self.toast(
                        format!("The elevated helper refused the request ({r:?})"),
                        toast::Level::Error,
                        now,
                    );
                }
                ElevatedEvent::Failed(msg) => {
                    self.toast(
                        format!("Could not get administrator rights: {msg}"),
                        toast::Level::Error,
                        now,
                    );
                }
            }
        }
    }

    /// Sends a retry to the elevated helper (started on first use: one UAC prompt).
    fn elevate(&mut self, r: Retry) {
        let Some(me) = self.me else {
            return;
        };
        let el = self.elevated.get_or_insert_with(|| {
            let ctx = self.repaint.clone();
            crate::elevate::Elevated::spawn(
                me,
                Box::new(move || {
                    if let Some(c) = &ctx {
                        c.request_repaint_after(std::time::Duration::from_millis(1));
                    }
                }),
            )
        });
        el.request(r);
    }

    /// Draws the dialog and toasts; sends the request when the user confirms.
    pub fn show(
        &mut self,
        ctx: &egui::Context,
        frame: &Frame,
        today: Option<LocalTime>,
        now: Instant,
    ) {
        if let Some(d) = &mut self.dialog {
            if let Some(answer) = dialog::show(ctx, d, frame, today) {
                let (key, kind, console) = (d.key, d.kind, d.console);
                self.dialog = None;
                if let Answer::Instead(supervisor) = answer {
                    self.start(supervisor, Act::KillTree, frame);
                }
                if let Answer::Confirm { force } = answer {
                    let opts = KillOpts {
                        force,
                        grace: self.grace,
                    };
                    let req = match kind {
                        Kind::KillTree if console => Request::KillConsole { key, opts },
                        Kind::Kill | Kind::KillTree => Request::Kill {
                            key,
                            tree: kind == Kind::KillTree,
                            opts,
                        },
                        Kind::Suspend => Request::Suspend { key, tree: false },
                    };
                    self.status.insert(key, "Starting…".to_owned());
                    self.send(req);
                }
            }
        }
        if let Some(g) = &self.group {
            if let Some(answer) = dialog::show_group(ctx, g, frame) {
                self.answer_group(answer);
            }
        }
        for r in self.toasts.show(ctx, now) {
            self.elevate(r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atropos_core::engine::policy::{HardReason, SoftReason};
    use atropos_core::platform::{FakePlatform, RawProc, Reaction};
    use std::time::Duration;

    #[test]
    fn denied_results_carry_a_retry() {
        use atropos_core::engine::kill::Outcome;
        use atropos_core::engine::subtree::TreeReport;
        use atropos_core::engine::suspend::Toggle;
        use atropos_helper::protocol::Op;
        let k = |pid| ProcKey {
            pid,
            create_time: 1,
        };
        assert_eq!(
            retry_for(&Event::Killed(k(1), Outcome::Denied)),
            Some(Retry {
                keys: vec![k(1)],
                op: Op::GracefulClose
            })
        );
        assert_eq!(retry_for(&Event::Killed(k(1), Outcome::Terminated)), None);
        let report = TreeReport {
            members: vec![
                (k(1), Outcome::Terminated),
                (k(2), Outcome::Denied),
                (k(3), Outcome::Denied),
            ],
        };
        assert_eq!(
            retry_for(&Event::TreeKilled(k(1), report)),
            Some(Retry {
                keys: vec![k(2), k(3)],
                op: Op::Terminate
            })
        );
        let toggled = Event::Toggled {
            key: k(1),
            suspend: false,
            results: vec![(k(1), Toggle::Denied, 0)],
        };
        assert_eq!(
            retry_for(&toggled),
            Some(Retry {
                keys: vec![k(1)],
                op: Op::Resume
            })
        );
    }

    #[test]
    fn risky_only_policy_skips_the_dialog_for_allowed_kills() {
        use atropos_core::config::Confirm;
        let soft = Verdict::Soft(SoftReason::Shell("explorer.exe".into()));
        let hard = Verdict::Hard(HardReason::SystemPid);
        assert_eq!(
            next_step_with(Act::Kill, &Verdict::Allowed, Confirm::Risky, false),
            Next::Send
        );
        assert_eq!(
            next_step_with(Act::KillTree, &soft, Confirm::Risky, false),
            Next::Dialog(Kind::KillTree)
        );
        assert_eq!(
            next_step_with(Act::Kill, &hard, Confirm::Risky, false),
            Next::Dialog(Kind::Kill)
        );
        assert_eq!(
            next_step_with(Act::Kill, &Verdict::Allowed, Confirm::Always, false),
            Next::Dialog(Kind::Kill)
        );
    }

    #[test]
    fn decision_table() {
        let soft = Verdict::Soft(SoftReason::Shell("explorer.exe".into()));
        let hard = Verdict::Hard(HardReason::SystemPid);
        assert_eq!(
            next_step(Act::Kill, &Verdict::Allowed),
            Next::Dialog(Kind::Kill)
        );
        assert_eq!(
            next_step(Act::KillTree, &soft),
            Next::Dialog(Kind::KillTree)
        );
        assert_eq!(
            next_step(Act::Kill, &hard),
            Next::Dialog(Kind::Kill),
            "the dialog explains"
        );
        assert_eq!(next_step(Act::Suspend, &Verdict::Allowed), Next::Send);
        assert_eq!(next_step(Act::Suspend, &soft), Next::Dialog(Kind::Suspend));
        assert_eq!(next_step(Act::Suspend, &hard), Next::Refuse);
        assert_eq!(next_step(Act::Resume, &Verdict::Allowed), Next::Send);
        assert_eq!(next_step(Act::Resume, &hard), Next::Refuse);
    }

    fn frame_with(key: ProcKey) -> Frame {
        let mut f = crate::test_frame::frame();
        let i = f.snapshot.index[&crate::test_frame::k(2)];
        f.snapshot.procs[i].raw.key = key;
        f.snapshot.index.insert(key, i);
        f
    }

    /// Drives the real executor (on the fake backend) until `done`.
    fn pump(a: &mut Actions, done: impl Fn(&Actions) -> bool) {
        for _ in 0..200 {
            a.poll(Instant::now());
            if done(a) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("timed out");
    }

    #[test]
    fn suspend_goes_straight_through_and_toasts() {
        let key = ProcKey {
            pid: 10,
            create_time: 1,
        };
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(key, 0, "node.exe")]);
        let exec = executor::spawn(
            Box::new(f),
            ProcKey {
                pid: 3,
                create_time: 3,
            },
            Box::new(|| {}),
        )
        .unwrap();
        let mut a = Actions::new(Some(exec));
        a.start(key, Act::Suspend, &frame_with(key));
        pump(&mut a, |a| a.suspend_count(key) == 1);
        assert!(!a.holds(), "no dialog for an allowed suspend");
        assert_eq!(a.toasts.len(), 1);
    }

    #[test]
    fn kill_opens_the_dialog_after_the_check() {
        let key = ProcKey {
            pid: 10,
            create_time: 1,
        };
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(key, 0, "node.exe")]);
        f.set_reaction(key, Reaction::IgnoresGraceful);
        let exec = executor::spawn(
            Box::new(f),
            ProcKey {
                pid: 3,
                create_time: 3,
            },
            Box::new(|| {}),
        )
        .unwrap();
        let mut a = Actions::new(Some(exec));
        a.start(key, Act::Kill, &frame_with(key));
        pump(&mut a, Actions::holds);
    }

    #[test]
    fn a_respawning_target_is_confirmed_even_in_risky_only_mode() {
        use atropos_core::config::Confirm;
        let allowed = Verdict::Allowed;
        assert_eq!(
            next_step_with(Act::Kill, &allowed, Confirm::Risky, false),
            Next::Send
        );
        assert_eq!(
            next_step_with(Act::Kill, &allowed, Confirm::Risky, true),
            Next::Dialog(Kind::Kill)
        );
    }

    #[test]
    fn group_kill_ends_each_allowed_tree_and_skips_the_protected() {
        use atropos_core::context::Ctx;
        use atropos_core::platform::SystemStats;
        use atropos_core::snapshot::build;
        let k = crate::test_frame::k;
        let me = k(3);
        let raws = vec![
            RawProc::new(k(1), 0, "explorer.exe"),
            RawProc::new(me, 1, "Atropos.exe"),
            RawProc::new(k(10), 1, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(12), 1, "python.exe"),
            RawProc::new(k(13), 1, "other.exe"),
        ];
        let mut f = FakePlatform::new(1);
        f.push_frame(raws.clone());
        let repo: std::sync::Arc<str> = "D:/code/shop".into();
        let mut frame = crate::test_frame::frame();
        frame.snapshot = build(None, raws, SystemStats::default());
        frame.statics = std::sync::Arc::default();
        frame.context = std::sync::Arc::new(
            [1, 10, 11, 12]
                .into_iter()
                .map(|pid| {
                    (
                        k(pid),
                        Ctx {
                            repo: Some(repo.clone()),
                            ..Ctx::default()
                        },
                    )
                })
                .collect(),
        );
        let exec = executor::spawn(Box::new(f), me, Box::new(|| {})).unwrap();
        let mut a = Actions::new(Some(exec));
        a.me = Some(me);
        a.start_group(&repo, &frame);
        assert!(a.holds());
        let plan = a.group().unwrap().plan.clone();
        assert_eq!(plan.roots, vec![k(10), k(12)]);
        assert_eq!(
            plan.skipped.len(),
            1,
            "explorer.exe needs its own confirmation"
        );
        a.answer_group(Answer::Confirm { force: true });
        assert!(!a.holds());
        pump(&mut a, |a| a.toasts.len() == 2);
    }

    #[test]
    fn cancelling_the_group_dialog_sends_nothing() {
        let mut a = Actions::new(None);
        a.start_group(&"nowhere".into(), &crate::test_frame::frame());
        assert!(!a.holds(), "an unknown repository opens nothing");
        let f = crate::test_frame::frame_with_context();
        a.start_group(&r"D:\code\shop".into(), &f);
        assert!(a.holds());
        a.answer_group(Answer::Cancel);
        assert!(!a.holds());
        assert!(a.status().is_empty());
    }
}
