//! Detail panel for the selected process: Overview and Environment (masked) tabs.

use std::collections::HashSet;

use atropos_core::collector::Frame;
use atropos_core::platform::ProcEnv;
use atropos_core::platform::windows::LocalTime;
use atropos_core::secrets::{Masked, mask};
use atropos_core::statics::Protection;
use atropos_core::{Field, ProcKey};
use eframe::egui::{self, RichText};

use crate::format;
use crate::table::{LOCK, RESTRICTED_TIP};

/// Shown instead of a hidden value.
pub const MASK: &str = "••••••";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Tab {
    #[default]
    Overview,
    Environment,
}

/// Panel state; revealed secrets are forgotten when the selection changes.
#[derive(Debug, Default)]
pub struct DetailsState {
    /// Whether the panel is shown (the selection survives closing it).
    pub open: bool,
    pub tab: Tab,
    revealed: HashSet<String>,
    for_key: Option<ProcKey>,
}

/// What the panel asks the app to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DetailEvent {
    Close,
}

/// Text for an unavailable field, with an explanation.
fn unavailable<T>(f: &Field<T>) -> (&'static str, &'static str) {
    match f {
        Field::Restricted => (LOCK, RESTRICTED_TIP),
        Field::Gone => ("—", "The process exited while it was being read"),
        Field::Unsupported | Field::Ok(_) => ("—", "Not available for this process"),
    }
}

fn field_row(ui: &mut egui::Ui, name: &str, f: &Field<std::sync::Arc<str>>) {
    ui.label(RichText::new(name).weak());
    match f {
        Field::Ok(v) => {
            ui.add(egui::Label::new(&**v).wrap());
        }
        other => {
            let (t, tip) = unavailable(other);
            ui.label(t).on_hover_text(tip);
        }
    }
    ui.end_row();
}

fn overview(ui: &mut egui::Ui, frame: &Frame, key: ProcKey, today: Option<LocalTime>) {
    let snap = &frame.snapshot;
    let Some(p) = snap.get(key) else {
        return;
    };
    let st = frame.statics.get(&key);
    egui::Grid::new("overview")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            ui.label(RichText::new("Name").weak());
            ui.label(&*p.raw.image);
            ui.end_row();
            ui.label(RichText::new("PID").weak());
            ui.label(key.pid.to_string());
            ui.end_row();
            let restricted = Field::Restricted;
            field_row(ui, "Executable", st.map_or(&restricted, |s| &s.exe_path));
            ui.label(RichText::new("Command line").weak());
            match st.map(|s| &s.cmdline) {
                Some(Field::Ok(cmd)) => {
                    ui.vertical(|ui| {
                        ui.add(egui::Label::new(RichText::new(&**cmd).monospace()).wrap());
                        if ui.small_button("Copy").clicked() {
                            ui.ctx().copy_text(cmd.to_string());
                        }
                    });
                }
                Some(other) => {
                    let (t, tip) = unavailable(other);
                    ui.label(t).on_hover_text(tip);
                }
                None => {
                    ui.label("—");
                }
            }
            ui.end_row();
            ui.label(RichText::new("Parents").weak());
            ui.add(egui::Label::new(parent_chain(frame, key)).wrap());
            ui.end_row();
            field_row(ui, "User", st.map_or(&restricted, |s| &s.user));
            ui.label(RichText::new("Session").weak());
            ui.label(p.raw.session.to_string());
            ui.end_row();
            ui.label(RichText::new("Started").weak());
            let started = atropos_core::platform::windows::local_time(key.create_time)
                .zip(today)
                .filter(|_| key.create_time > 0)
                .map_or_else(|| "—".to_owned(), |(t, d)| format::started(t, d));
            ui.label(started);
            ui.end_row();
            ui.label(RichText::new("Protection").weak());
            match st.map(|s| &s.protection) {
                Some(Field::Ok(pr)) => {
                    ui.label(match pr {
                        Protection::None => "None",
                        Protection::Light => "Protected (light)",
                        Protection::Full => "Protected",
                    });
                }
                Some(other) => {
                    let (t, tip) = unavailable(other);
                    ui.label(t).on_hover_text(tip);
                }
                None => {
                    ui.label("—");
                }
            }
            ui.end_row();
            ui.label(RichText::new("Architecture").weak());
            ui.label(match st.map(|s| &s.wow64) {
                Some(Field::Ok(true)) => "32-bit (WOW64)",
                Some(Field::Ok(false)) => "Native",
                _ => "—",
            });
            ui.end_row();
        });
}

/// `explorer.exe (5408) › node.exe (1234)`: validated ancestors, oldest first.
#[must_use]
pub fn parent_chain(frame: &Frame, key: ProcKey) -> String {
    let snap = &frame.snapshot;
    let mut chain = Vec::new();
    let mut up = snap.index.get(&key).and_then(|&i| snap.procs[i].parent);
    while let Some(i) = up {
        let p = &snap.procs[i];
        chain.push(format!("{} ({})", p.raw.image, p.raw.key.pid));
        up = p.parent;
    }
    if chain.is_empty() {
        return "—".to_owned();
    }
    chain.reverse();
    chain.join(" › ")
}

fn environment(ui: &mut egui::Ui, env: Option<&Field<ProcEnv>>, revealed: &mut HashSet<String>) {
    let e = match env {
        None => {
            ui.label("Reading…");
            return;
        }
        Some(Field::Ok(e)) => e,
        Some(other) => {
            let (t, tip) = unavailable(other);
            ui.horizontal(|ui| {
                ui.label(t).on_hover_text(tip);
                ui.label("The environment of this process is not readable from your account.");
            });
            return;
        }
    };
    ui.horizontal(|ui| {
        ui.label(RichText::new("Working directory").weak());
        ui.add(egui::Label::new(&*e.cwd).wrap());
    });
    ui.separator();
    egui::Grid::new("env")
        .num_columns(2)
        .striped(true)
        .show(ui, |ui| {
            for (name, value) in &e.vars {
                ui.label(RichText::new(name).monospace());
                let hidden = mask(name, value) == Masked::Hidden && !revealed.contains(name);
                if hidden {
                    ui.horizontal(|ui| {
                        ui.label(MASK)
                            .on_hover_text("Looks like a secret (name or value pattern)");
                        if ui.small_button("Show").clicked() {
                            revealed.insert(name.clone());
                        }
                    });
                } else {
                    ui.add(egui::Label::new(RichText::new(value).monospace()).wrap());
                }
                ui.end_row();
            }
        });
}

/// Draws the panel for `key`.
pub fn show(
    ui: &mut egui::Ui,
    frame: &Frame,
    key: ProcKey,
    state: &mut DetailsState,
    today: Option<LocalTime>,
) -> Option<DetailEvent> {
    if state.for_key != Some(key) {
        state.revealed.clear();
        state.for_key = Some(key);
    }
    let mut event = None;
    ui.horizontal(|ui| {
        ui.selectable_value(&mut state.tab, Tab::Overview, "Overview");
        ui.selectable_value(&mut state.tab, Tab::Environment, "Environment");
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            if ui.small_button("✖").on_hover_text("Close (Esc)").clicked() {
                event = Some(DetailEvent::Close);
            }
        });
    });
    ui.separator();
    egui::ScrollArea::vertical().show(ui, |ui| match state.tab {
        Tab::Overview => overview(ui, frame, key, today),
        Tab::Environment => {
            let env = frame
                .env
                .as_ref()
                .filter(|(k, _)| *k == key)
                .map(|(_, e)| &**e);
            environment(ui, env, &mut state.revealed);
        }
    });
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, k};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;
    use std::sync::Arc;

    #[test]
    fn parent_chain_lists_validated_ancestors_oldest_first() {
        let f = frame();
        assert_eq!(parent_chain(&f, k(4)), "explorer.exe (1) › node.exe (2)");
        assert_eq!(parent_chain(&f, k(1)), "—");
    }

    fn with_env(vars: &[(&str, &str)]) -> Frame {
        let mut f = frame();
        f.env = Some((
            k(2),
            Arc::new(Field::Ok(ProcEnv {
                cwd: Arc::from(r"C:\src\shop\"),
                vars: vars
                    .iter()
                    .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
                    .collect(),
            })),
        ));
        f
    }

    fn harness(f: Frame) -> Harness<'static, (Frame, DetailsState)> {
        let state = DetailsState {
            tab: Tab::Environment,
            ..DetailsState::default()
        };
        Harness::new_ui_state(
            |ui, (f, st): &mut (Frame, DetailsState)| {
                show(ui, f, k(2), st, None);
            },
            (f, state),
        )
    }

    #[test]
    fn environment_values_are_masked_until_revealed() {
        let mut h = harness(with_env(&[
            ("PORT", "3000"),
            ("GITHUB_TOKEN", "ghp_secret"),
        ]));
        h.run();
        assert!(h.query_by_label("3000").is_some());
        assert!(h.query_by_label("ghp_secret").is_none());
        assert!(h.query_by_label(MASK).is_some());
        h.get_by_label("Show").click();
        h.run();
        assert!(h.query_by_label("ghp_secret").is_some());
        assert!(h.query_by_label(r"C:\src\shop\").is_some());
    }

    #[test]
    fn environment_restricted_explains_itself() {
        let mut f = frame();
        f.env = Some((k(2), Arc::new(Field::Restricted)));
        let mut h = harness(f);
        h.run();
        assert!(h.query_by_label(LOCK).is_some());
    }

    #[test]
    fn revealed_secrets_are_forgotten_on_selection_change() {
        let mut st = DetailsState::default();
        st.revealed.insert("GITHUB_TOKEN".into());
        st.for_key = Some(k(2));
        let f = frame();
        let mut h = Harness::new_ui_state(
            |ui, st: &mut DetailsState| {
                show(ui, &f, k(3), st, None);
            },
            st,
        );
        h.run();
        assert!(h.state().revealed.is_empty());
    }
}
