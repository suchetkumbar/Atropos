//! "Set affinity…" (spec Phase 6): which logical CPUs a process may run on, typed as `0-3,6`.

use atropos_core::ProcKey;
use atropos_core::tune::{format_affinity, parse_affinity};
use eframe::egui;

/// The open window: for which process, the text being edited, and why it was refused.
#[derive(Debug)]
pub struct AffinityUi {
    pub key: ProcKey,
    pub name: String,
    pub cpus: u32,
    text: String,
    err: Option<String>,
}

impl AffinityUi {
    /// Opens with the current mask (`None` when it could not be read: every CPU).
    #[must_use]
    pub fn new(key: ProcKey, name: String, cpus: u32, current: Option<u64>) -> Self {
        let all = if cpus >= 64 {
            u64::MAX
        } else {
            (1u64 << cpus) - 1
        };
        Self {
            key,
            name,
            cpus,
            text: format_affinity(current.unwrap_or(all)),
            err: None,
        }
    }

    /// Shows a refusal from Windows under the field.
    pub fn fail(&mut self, e: String) {
        self.err = Some(e);
    }
}

/// What the window asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AffinityEvent {
    Apply(ProcKey, u64),
    Close,
}

/// Draws the window.
pub fn show(ctx: &egui::Context, st: &mut AffinityUi) -> Option<AffinityEvent> {
    let mut out = None;
    let mut open = true;
    egui::Window::new("Set affinity")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(format!(
                "CPUs {} ({}) may run on, e.g. 0-3,6 (this machine: 0-{})",
                st.name,
                st.key.pid,
                st.cpus.saturating_sub(1)
            ));
            let r = ui.text_edit_singleline(&mut st.text);
            let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if let Some(e) = &st.err {
                ui.colored_label(ui.visuals().error_fg_color, e);
            }
            ui.horizontal(|ui| {
                if ui.button("Apply").clicked() || enter {
                    match parse_affinity(&st.text, st.cpus) {
                        Ok(mask) => out = Some(AffinityEvent::Apply(st.key, mask)),
                        Err(e) => st.err = Some(e),
                    }
                }
                if ui.button("Cancel").clicked() {
                    out = Some(AffinityEvent::Close);
                }
            });
        });
    if !open {
        out = Some(AffinityEvent::Close);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct S {
        ui: AffinityUi,
        out: Vec<AffinityEvent>,
    }

    fn harness(current: Option<u64>) -> Harness<'static, S> {
        let key = ProcKey {
            pid: 7,
            create_time: 7,
        };
        Harness::builder()
            .with_size(egui::vec2(600.0, 300.0))
            .build_state(
                |ctx, s: &mut S| {
                    if let Some(e) = show(ctx, &mut s.ui) {
                        s.out.push(e);
                    }
                },
                S {
                    ui: AffinityUi::new(key, "node.exe".into(), 8, current),
                    out: Vec::new(),
                },
            )
    }

    #[test]
    fn the_current_mask_is_shown_and_applied() {
        let mut h = harness(Some(0b1111));
        h.run();
        assert_eq!(h.state().ui.text, "0-3");
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(
            h.state().out,
            vec![AffinityEvent::Apply(
                ProcKey {
                    pid: 7,
                    create_time: 7
                },
                0b1111
            )]
        );
    }

    #[test]
    fn a_cpu_the_machine_lacks_is_refused_in_place() {
        let mut h = harness(None);
        h.run();
        h.state_mut().ui.text = "9".into();
        h.run();
        h.get_by_label("Apply").click();
        h.run();
        assert_eq!(h.state().out, Vec::new());
        assert!(h.query_by_label("CPUs are numbered 0-7").is_some());
    }
}
