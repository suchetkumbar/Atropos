//! After a crash: offer to resume the processes Atropos left suspended (from the journal).

use atropos_core::ProcKey;
use eframe::egui;

/// The processes left suspended, with names for display.
#[derive(Debug, Clone)]
pub struct ResumeOffer {
    pub items: Vec<(ProcKey, u32, String)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Choice {
    ResumeAll,
    Leave,
}

/// Draws the offer; returns the user's choice once made.
pub fn show(ctx: &egui::Context, offer: &ResumeOffer) -> Option<Choice> {
    let mut choice = None;
    egui::Modal::new(egui::Id::new("atropos-resume-offer")).show(ctx, |ui| {
        ui.set_width(420.0);
        ui.heading("Resume suspended processes?");
        ui.label(format!(
            "Atropos closed unexpectedly while {} processes it had suspended were still frozen:",
            offer.items.len()
        ));
        for (key, n, name) in &offer.items {
            let nested = if *n > 1 {
                format!(" (×{n})")
            } else {
                String::new()
            };
            ui.label(format!("• {name} ({}){nested}", key.pid));
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            if ui.button("Resume all").clicked() {
                choice = Some(Choice::ResumeAll);
            }
            if ui.button("Leave suspended").clicked() {
                choice = Some(Choice::Leave);
            }
        });
    });
    choice
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    fn offer() -> ResumeOffer {
        ResumeOffer {
            items: vec![
                (
                    ProcKey {
                        pid: 10,
                        create_time: 1,
                    },
                    1,
                    "node.exe".into(),
                ),
                (
                    ProcKey {
                        pid: 11,
                        create_time: 2,
                    },
                    2,
                    "vite.exe".into(),
                ),
            ],
        }
    }

    fn harness() -> Harness<'static, Option<Choice>> {
        let o = offer();
        Harness::new_state(
            move |ctx, c: &mut Option<Choice>| {
                if let Some(x) = show(ctx, &o) {
                    *c = Some(x);
                }
            },
            None,
        )
    }

    #[test]
    fn lists_the_processes_and_resumes_all() {
        let mut h = harness();
        h.run();
        assert!(h.query_by_label_contains("2 processes").is_some());
        assert!(h.query_by_label_contains("node.exe (10)").is_some());
        assert!(h.query_by_label_contains("vite.exe (11)").is_some());
        h.get_by_label("Resume all").click();
        h.run();
        assert_eq!(*h.state(), Some(Choice::ResumeAll));
    }

    #[test]
    fn can_leave_them_suspended() {
        let mut h = harness();
        h.run();
        h.get_by_label("Leave suspended").click();
        h.run();
        assert_eq!(*h.state(), Some(Choice::Leave));
    }
}
