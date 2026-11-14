//! Confirmation dialog for destructive actions (spec Phase 2): who (label, PID, start time), how
//! much (subtree count, memory freed), warnings, the strategy, and the keys `Enter` (graceful),
//! `F` (force), `Esc` (cancel). Soft-blocked targets need the exact name typed first; hard-blocked
//! targets only get an explanation.

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::engine::policy::{HardReason, SoftReason, Verdict};
use atropos_core::platform::windows::LocalTime;
use eframe::egui::{self, RichText};

use crate::format;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Kill,
    KillTree,
    Suspend,
}

/// An open dialog.
#[derive(Debug, Clone)]
pub struct Dialog {
    pub key: ProcKey,
    pub kind: Kind,
    pub verdict: Verdict,
    typed: String,
}

impl Dialog {
    #[must_use]
    pub fn new(key: ProcKey, kind: Kind, verdict: Verdict) -> Self {
        Self {
            key,
            kind,
            verdict,
            typed: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    Cancel,
    Confirm { force: bool },
}

/// Why a soft block needs confirmation, in plain words.
#[must_use]
pub fn soft_text(r: &SoftReason) -> String {
    match r {
        SoftReason::Shell(name) => format!(
            "{name} is part of the Windows desktop; it may restart or leave the screen blank."
        ),
        SoftReason::MultiServiceHost(n) => {
            format!("This svchost.exe hosts {n} Windows services; they all stop.")
        }
        SoftReason::OtherSession(s) => {
            format!("This process runs in another session ({s}): another user or a system service.")
        }
    }
}

/// Why a hard block refuses, in plain words.
#[must_use]
pub fn hard_text(r: &HardReason) -> String {
    match r {
        HardReason::SystemPid => "This is a core part of Windows.".to_owned(),
        HardReason::Critical(name) => {
            format!("{name} is critical to Windows; ending it would crash or log off the system.")
        }
        HardReason::SelfOrAncestor => {
            "This is Atropos itself, or a process Atropos was started from.".to_owned()
        }
        HardReason::Protected => "Windows protects this process; it cannot be stopped.".to_owned(),
    }
}

/// `Esc` cancels; `Enter` confirms and `F` forces when confirmation is allowed (`F` not while
/// typing the name).
fn key_answer(ctx: &egui::Context, confirm_ok: bool, suspend: bool) -> Option<Answer> {
    let typing = ctx.wants_keyboard_input();
    ctx.input(|i| {
        if i.key_pressed(egui::Key::Escape) {
            Some(Answer::Cancel)
        } else if confirm_ok && i.key_pressed(egui::Key::Enter) {
            Some(Answer::Confirm { force: false })
        } else if confirm_ok && !suspend && !typing && i.key_pressed(egui::Key::F) {
            Some(Answer::Confirm { force: true })
        } else {
            None
        }
    })
}

/// Draws the dialog; returns the user's answer once given.
pub fn show(
    ctx: &egui::Context,
    d: &mut Dialog,
    frame: &Frame,
    today: Option<LocalTime>,
) -> Option<Answer> {
    let snap = &frame.snapshot;
    let p = snap.get(d.key);
    let image = p.map_or_else(|| "this process".to_owned(), |p| p.raw.image.to_string());
    let (soft, hard) = match &d.verdict {
        Verdict::Soft(r) => (Some(r.clone()), None),
        Verdict::Hard(r) => (None, Some(r.clone())),
        Verdict::Allowed => (None, None),
    };
    let tree = d.kind == Kind::KillTree;
    let suspend = d.kind == Kind::Suspend;
    let confirm_ok = hard.is_none() && soft.as_ref().is_none_or(|_| d.typed == image);
    let mut answer = None;
    let modal = egui::Modal::new(egui::Id::new("atropos-confirm")).show(ctx, |ui| {
        ui.set_width(440.0);
        ui.heading(match d.kind {
            Kind::Kill => "End process",
            Kind::KillTree => "End process tree",
            Kind::Suspend => "Suspend process",
        });
        let started = atropos_core::platform::windows::local_time(d.key.create_time)
            .zip(today)
            .map_or_else(String::new, |(t, td)| {
                format!(" · started {}", format::started(t, td))
            });
        ui.label(RichText::new(format!("{image} · PID {}{started}", d.key.pid)).strong());
        if let Some(h) = &hard {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().error_fg_color, hard_text(h));
            ui.add_space(6.0);
            if ui.button("Close").clicked() {
                answer = Some(Answer::Cancel);
            }
            return;
        }
        if let Some(p) = p {
            if tree {
                ui.label(format!("{} processes in this tree", p.rollup.count));
            }
            let mem = if tree {
                p.rollup.private_ws
            } else {
                p.raw.private_ws
            };
            #[allow(clippy::cast_precision_loss)] // display only
            let frac = mem as f32 / snap.stats.total_ram.max(1) as f32;
            ui.label(format!(
                "Frees about {} ({:.1} % of memory)",
                format::bytes(mem),
                frac * 100.0
            ));
            ui.add(egui::ProgressBar::new(frac.clamp(0.0, 1.0)).desired_height(6.0));
        }
        ui.add_space(4.0);
        ui.label(if suspend {
            "Freezes it until you resume it (Ctrl+P)."
        } else {
            "Asks it to close first (service stop, window close or Ctrl+C), waits 5 s, then forces."
        });
        if let Some(r) = &soft {
            ui.add_space(6.0);
            ui.colored_label(ui.visuals().warn_fg_color, soft_text(r));
            ui.label(format!("Type {image} to confirm:"));
            ui.text_edit_singleline(&mut d.typed);
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let main = if suspend { "Suspend" } else { "End" };
            if ui
                .add_enabled(confirm_ok, egui::Button::new(main))
                .on_hover_text("Enter")
                .clicked()
            {
                answer = Some(Answer::Confirm { force: false });
            }
            if !suspend
                && ui
                    .add_enabled(confirm_ok, egui::Button::new("Force"))
                    .on_hover_text("F: skip the graceful step")
                    .clicked()
            {
                answer = Some(Answer::Confirm { force: true });
            }
            if ui.button("Cancel").on_hover_text("Esc").clicked() {
                answer = Some(Answer::Cancel);
            }
        });
    });
    if answer.is_none() {
        answer = key_answer(ctx, confirm_ok, suspend);
    }
    if answer.is_none() && modal.should_close() {
        answer = Some(Answer::Cancel);
    }
    answer
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, k};
    use eframe::egui::Key;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct S {
        d: Dialog,
        f: Frame,
        answer: Option<Answer>,
    }

    fn harness(d: Dialog) -> Harness<'static, S> {
        Harness::builder()
            .with_size(egui::vec2(800.0, 600.0))
            .build_state(
                |ctx, s: &mut S| {
                    if let Some(a) = show(ctx, &mut s.d, &s.f, None) {
                        s.answer = Some(a);
                    }
                },
                S {
                    d,
                    f: frame(),
                    answer: None,
                },
            )
    }

    #[test]
    fn shows_who_how_much_and_the_strategy() {
        let mut h = harness(Dialog::new(k(2), Kind::KillTree, Verdict::Allowed));
        h.run();
        assert!(h.query_by_label_contains("node.exe").is_some());
        assert!(h.query_by_label_contains("PID 2").is_some());
        assert!(h.query_by_label_contains("2 processes").is_some());
        assert!(h.query_by_label_contains("Frees about").is_some());
        assert!(h.query_by_label_contains("then forces").is_some());
    }

    #[test]
    fn enter_confirms_f_forces_esc_cancels() {
        for (key, want) in [
            (Key::Enter, Answer::Confirm { force: false }),
            (Key::F, Answer::Confirm { force: true }),
            (Key::Escape, Answer::Cancel),
        ] {
            let mut h = harness(Dialog::new(k(3), Kind::Kill, Verdict::Allowed));
            h.run();
            h.key_press(key);
            h.run();
            assert_eq!(h.state().answer, Some(want), "{key:?}");
        }
    }

    #[test]
    fn soft_block_requires_the_exact_name() {
        let v = Verdict::Soft(SoftReason::Shell("explorer.exe".into()));
        let mut h = harness(Dialog::new(k(1), Kind::Kill, v));
        h.run();
        assert!(h.query_by_label_contains("Windows desktop").is_some());
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(
            h.state().answer,
            None,
            "Enter does nothing before the name is typed"
        );
        h.state_mut().d.typed = "Explorer.exe".into();
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().answer, None, "case must match exactly");
        h.state_mut().d.typed = "explorer.exe".into();
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().answer, Some(Answer::Confirm { force: false }));
    }

    #[test]
    fn hard_block_only_explains() {
        let v = Verdict::Hard(HardReason::Critical("lsass.exe".into()));
        let mut h = harness(Dialog::new(k(9), Kind::Kill, v));
        h.run();
        assert!(h.query_by_label_contains("critical to Windows").is_some());
        assert!(h.query_by_label("End").is_none());
        h.key_press(Key::Enter);
        h.key_press(Key::F);
        h.run();
        assert_ne!(h.state().answer, Some(Answer::Confirm { force: false }));
        assert_ne!(h.state().answer, Some(Answer::Confirm { force: true }));
    }
}
