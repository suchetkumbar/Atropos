//! Outcome toasts and the inline row status (`Closing… 3s`).

use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::engine::kill::{Outcome, Progress};
use atropos_core::engine::subtree::TreeReport;
use atropos_core::engine::suspend::Toggle;
use eframe::egui;

/// How long a toast stays (errors stay longer).
pub const LIFETIME: Duration = Duration::from_secs(5);
pub const ERROR_LIFETIME: Duration = Duration::from_secs(10);
pub const RETRY_ADMIN: &str = "Retry as administrator";
pub const RETRY_ADMIN_TIP: &str = "Elevation arrives in Atropos v0.1 (Phase 3).";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Ok,
    Warn,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    pub text: String,
    pub level: Level,
    /// Access was denied: offer elevation (disabled until Phase 3).
    pub offer_admin: bool,
}

fn msg(text: String, level: Level) -> Message {
    Message {
        text,
        level,
        offer_admin: false,
    }
}

/// Inline row status while an action runs.
#[must_use]
pub fn progress_text(p: &Progress) -> String {
    match p {
        Progress::Closing { left, .. } => format!("Closing… {}s", left.as_secs()),
        Progress::Forcing => "Forcing…".to_owned(),
    }
}

/// Toast for a single-process kill.
#[must_use]
pub fn outcome_message(image: &str, pid: u32, o: Outcome) -> Message {
    let who = format!("{image} ({pid})");
    match o {
        Outcome::Exited => msg(format!("{who} closed"), Level::Ok),
        Outcome::Terminated => msg(format!("{who} was force-terminated"), Level::Ok),
        Outcome::Gone => msg(format!("{who} had already exited"), Level::Ok),
        Outcome::Protected => msg(format!("{who} is protected; left running"), Level::Warn),
        Outcome::Stuck => msg(
            format!("{who} is still running after being terminated (stuck)"),
            Level::Error,
        ),
        Outcome::Denied => Message {
            text: format!("{who}: access denied"),
            level: Level::Error,
            offer_admin: true,
        },
    }
}

/// Toast for a subtree kill.
#[must_use]
pub fn tree_message(image: &str, pid: u32, r: &TreeReport) -> Message {
    let bad = r.count(Outcome::Stuck) + r.count(Outcome::Denied);
    let level = if bad > 0 {
        Level::Error
    } else if r.count(Outcome::Protected) > 0 {
        Level::Warn
    } else {
        Level::Ok
    };
    Message {
        text: format!("{image} ({pid}) tree: {}", r.summary()),
        level,
        offer_admin: r.count(Outcome::Denied) > 0,
    }
}

/// Toast for suspend / resume of `key` (first result is the target itself).
#[must_use]
pub fn toggle_message(
    image: &str,
    key: ProcKey,
    suspend: bool,
    results: &[(ProcKey, Toggle, u32)],
) -> Message {
    let who = format!("{image} ({})", key.pid);
    let verb = if suspend { "suspended" } else { "resumed" };
    let failed = |t: Toggle| results.iter().filter(|(_, r, _)| *r == t).count();
    let denied = failed(Toggle::Denied);
    if denied > 0 {
        return Message {
            text: format!("{who}: access denied ({denied} not {verb})"),
            level: Level::Error,
            offer_admin: true,
        };
    }
    if failed(Toggle::Protected) > 0 {
        return msg(format!("{who} is protected; not {verb}"), Level::Warn);
    }
    if results.first().is_some_and(|(_, t, _)| *t == Toggle::Gone) {
        return msg(format!("{who} had already exited"), Level::Ok);
    }
    let count = results.first().map_or(0, |(_, _, c)| *c);
    let extra = if results.len() > 1 {
        format!(" with {} descendants", results.len() - 1)
    } else {
        String::new()
    };
    let nested = if suspend && count > 1 {
        format!(" (×{count})")
    } else {
        String::new()
    };
    msg(format!("{who} {verb}{extra}{nested}"), Level::Ok)
}

struct Toast {
    msg: Message,
    born: Instant,
}

impl Toast {
    fn lifetime(&self) -> Duration {
        if self.msg.level == Level::Error {
            ERROR_LIFETIME
        } else {
            LIFETIME
        }
    }
}

/// The toast stack (bottom-right).
#[derive(Default)]
pub struct Toasts {
    items: Vec<Toast>,
}

impl Toasts {
    pub fn push(&mut self, msg: Message, now: Instant) {
        self.items.push(Toast { msg, born: now });
    }

    #[cfg(test)]
    #[must_use]
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Draws live toasts and drops expired ones; asks for a repaint when the next one expires.
    pub fn show(&mut self, ctx: &egui::Context, now: Instant) {
        self.items
            .retain(|t| now.saturating_duration_since(t.born) < t.lifetime());
        let Some(next) = self
            .items
            .iter()
            .map(|t| {
                t.lifetime()
                    .saturating_sub(now.saturating_duration_since(t.born))
            })
            .min()
        else {
            return;
        };
        ctx.request_repaint_after(next);
        egui::Area::new(egui::Id::new("atropos-toasts"))
            .anchor(egui::Align2::RIGHT_BOTTOM, egui::vec2(-12.0, -12.0))
            .show(ctx, |ui| {
                for t in &self.items {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        let color = match t.msg.level {
                            Level::Ok => ui.visuals().text_color(),
                            Level::Warn => ui.visuals().warn_fg_color,
                            Level::Error => ui.visuals().error_fg_color,
                        };
                        ui.colored_label(color, &t.msg.text);
                        if t.msg.offer_admin {
                            ui.add_enabled(false, egui::Button::new(RETRY_ADMIN))
                                .on_disabled_hover_text(RETRY_ADMIN_TIP);
                        }
                    });
                    ui.add_space(4.0);
                }
            });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::{NodeT as _, Queryable as _};
    use std::time::Duration as D;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: 1,
        }
    }

    #[test]
    fn progress_text_counts_down() {
        assert_eq!(
            progress_text(&Progress::Closing {
                strategy: atropos_core::engine::kill::Strategy::Window,
                left: D::from_secs(3)
            }),
            "Closing… 3s"
        );
        assert_eq!(progress_text(&Progress::Forcing), "Forcing…");
    }

    #[test]
    fn every_outcome_has_an_honest_message() {
        let m = |o| outcome_message("node.exe", 42, o);
        assert_eq!(m(Outcome::Exited).text, "node.exe (42) closed");
        assert_eq!(
            m(Outcome::Terminated).text,
            "node.exe (42) was force-terminated"
        );
        assert_eq!(m(Outcome::Gone).text, "node.exe (42) had already exited");
        assert!(m(Outcome::Protected).text.contains("protected"));
        assert!(m(Outcome::Stuck).text.contains("still running"));
        assert_eq!(m(Outcome::Stuck).level, Level::Error);
        let denied = m(Outcome::Denied);
        assert!(denied.text.contains("access denied"));
        assert!(denied.offer_admin);
        assert!(!m(Outcome::Terminated).offer_admin);
    }

    #[test]
    fn tree_message_uses_the_report_summary() {
        let r = TreeReport {
            members: vec![(k(1), Outcome::Exited), (k(2), Outcome::Denied)],
        };
        let m = tree_message("node.exe", 1, &r);
        assert_eq!(
            m.text,
            "node.exe (1) tree: 1 exited · 0 terminated · 0 stuck · 1 denied"
        );
        assert_eq!(m.level, Level::Error);
        assert!(m.offer_admin);
    }

    #[test]
    fn toggle_message_shows_nesting_and_failures() {
        let m = toggle_message("node.exe", k(5), true, &[(k(5), Toggle::Done, 2)]);
        assert_eq!(m.text, "node.exe (5) suspended (×2)");
        let m = toggle_message("node.exe", k(5), false, &[(k(5), Toggle::Done, 0)]);
        assert_eq!(m.text, "node.exe (5) resumed");
        let m = toggle_message("svc.exe", k(5), true, &[(k(5), Toggle::Denied, 0)]);
        assert!(m.text.contains("access denied") && m.offer_admin);
    }

    #[test]
    fn denied_toast_offers_a_disabled_retry_as_administrator() {
        let mut h = Harness::new_state(|ctx, t: &mut Toasts| t.show(ctx, Instant::now()), {
            let mut t = Toasts::default();
            t.push(
                outcome_message("svc.exe", 7, Outcome::Denied),
                Instant::now(),
            );
            t
        });
        h.run();
        assert!(h.query_by_label_contains("access denied").is_some());
        let retry = h.get_by_label(RETRY_ADMIN);
        assert!(retry.accesskit_node().is_disabled());
    }

    #[test]
    fn toasts_expire() {
        let t0 = Instant::now();
        let mut toasts = Toasts::default();
        toasts.push(outcome_message("a.exe", 1, Outcome::Exited), t0);
        toasts.push(outcome_message("b.exe", 2, Outcome::Stuck), t0);
        let ctx = egui::Context::default();
        let _ = ctx.run(egui::RawInput::default(), |ctx| {
            toasts.show(ctx, t0 + LIFETIME + D::from_millis(1));
        });
        assert_eq!(toasts.len(), 1, "the error toast stays longer");
    }
}
