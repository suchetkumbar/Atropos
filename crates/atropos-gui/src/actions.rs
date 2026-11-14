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

use crate::dialog::{self, Answer, Dialog, Kind};
use crate::toast::{self, Toasts};

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
    /// The act a pending check is for.
    pending: HashMap<ProcKey, Act>,
    /// Names captured when the action started (the process may be gone by the end).
    names: HashMap<ProcKey, String>,
    /// Inline row status (`Closing… 3s`).
    status: HashMap<ProcKey, String>,
    /// Atropos's outstanding suspends per process.
    counts: HashMap<ProcKey, u32>,
    pub toasts: Toasts,
}

impl Actions {
    #[must_use]
    pub fn new(exec: Option<Executor>) -> Self {
        Self {
            exec,
            dialog: None,
            pending: HashMap::new(),
            names: HashMap::new(),
            status: HashMap::new(),
            counts: HashMap::new(),
            toasts: Toasts::default(),
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
        let ctx = ctx.clone();
        let exec = executor::spawn(
            Box::new(platform),
            me,
            Box::new(move || ctx.request_repaint_after(std::time::Duration::from_millis(1))),
        )
        .inspect_err(|e| tracing::error!("executor thread: {e}"))
        .ok();
        Self::new(exec)
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
        self.dialog.is_some()
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
        let events: Vec<Event> = match &self.exec {
            Some(e) => e.events.try_iter().collect(),
            None => return,
        };
        for ev in events {
            match ev {
                Event::Progress(key, p) => {
                    self.status.insert(key, toast::progress_text(&p));
                }
                Event::Killed(key, o) => {
                    self.status.remove(&key);
                    let m = toast::outcome_message(&self.name(key), key.pid, o);
                    self.toasts.push(m, now);
                }
                Event::TreeKilled(key, r) => {
                    self.status.remove(&key);
                    let m = toast::tree_message(&self.name(key), key.pid, &r);
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
                    let m = toast::toggle_message(&self.name(key), key, suspend, &results);
                    self.toasts.push(m, now);
                }
                Event::Checked { key, kind, verdict } => {
                    let act = self.pending.remove(&key).unwrap_or(match kind {
                        ActionKind::Kill => Act::Kill,
                        ActionKind::KillTree => Act::KillTree,
                        ActionKind::Suspend => Act::Suspend,
                        ActionKind::Resume => Act::Resume,
                    });
                    match next_step(act, &verdict) {
                        Next::Dialog(k) => self.dialog = Some(Dialog::new(key, k, verdict)),
                        Next::Send => self.send(if act == Act::Resume {
                            Request::Resume { key, tree: false }
                        } else {
                            Request::Suspend { key, tree: false }
                        }),
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
                                },
                                now,
                            );
                        }
                    }
                }
            }
        }
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
                let (key, kind) = (d.key, d.kind);
                self.dialog = None;
                if let Answer::Confirm { force } = answer {
                    let req = match kind {
                        Kind::Kill | Kind::KillTree => Request::Kill {
                            key,
                            tree: kind == Kind::KillTree,
                            opts: KillOpts {
                                force,
                                ..KillOpts::default()
                            },
                        },
                        Kind::Suspend => Request::Suspend { key, tree: false },
                    };
                    self.status.insert(key, "Starting…".to_owned());
                    self.send(req);
                }
            }
        }
        self.toasts.show(ctx, now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atropos_core::engine::policy::{HardReason, SoftReason};
    use atropos_core::platform::{FakePlatform, RawProc, Reaction};
    use std::time::Duration;

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
}
