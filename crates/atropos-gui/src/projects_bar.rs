//! The Projects strip (spec §4.8): one chip per git repository that has running processes.
//! Picking a chip narrows the table to that repository and offers "End everything from …".

use std::sync::Arc;

use atropos_core::projects::Group;
use eframe::egui;

/// What the user did in the strip this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BarEvent {
    /// Show only this repository (`None` = all of them again).
    Pick(Option<Arc<str>>),
    /// Open the confirmation for ending every process of this repository.
    KillAll(Arc<str>),
}

/// Button text for the group action.
#[must_use]
pub fn kill_all_text(name: &str) -> String {
    format!("End everything from {name}…")
}

/// Draws the strip; `selected` is the repository the table is narrowed to.
pub fn show(ui: &mut egui::Ui, groups: &[Group], selected: Option<&Arc<str>>) -> Option<BarEvent> {
    // The picked repository has no processes any more: show all projects again.
    let mut event = selected
        .filter(|sel| !groups.iter().any(|g| g.repo == **sel))
        .map(|_| BarEvent::Pick(None));
    ui.horizontal_wrapped(|ui| {
        if groups.is_empty() {
            ui.label(
                "No process is running inside a git repository (still looking after start-up).",
            );
            return;
        }
        for g in groups {
            let on = selected == Some(&g.repo);
            let chip = ui
                .selectable_label(on, format!("{} ({})", g.name, g.members.len()))
                .on_hover_text(&*g.repo);
            if chip.clicked() {
                event = Some(BarEvent::Pick((!on).then(|| Arc::clone(&g.repo))));
            }
        }
        if let Some(g) = groups.iter().find(|g| selected == Some(&g.repo)) {
            ui.separator();
            if ui
                .button(kill_all_text(&g.name))
                .on_hover_text("Ends each process tree of this repository, after you confirm")
                .clicked()
            {
                event = Some(BarEvent::KillAll(Arc::clone(&g.repo)));
            }
        }
    });
    event
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::k;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct S {
        groups: Vec<Group>,
        selected: Option<Arc<str>>,
        events: Vec<BarEvent>,
    }

    fn harness(selected: Option<&str>) -> Harness<'static, S> {
        let groups = vec![
            Group {
                repo: Arc::from(r"E:\Atropos"),
                name: "Atropos".into(),
                members: vec![k(9)],
            },
            Group {
                repo: Arc::from(r"D:\code\shop"),
                name: "shop".into(),
                members: vec![k(2), k(4)],
            },
        ];
        Harness::builder()
            .with_size(egui::vec2(900.0, 120.0))
            .build_ui_state(
                |ui, s: &mut S| {
                    if let Some(e) = show(ui, &s.groups, s.selected.as_ref()) {
                        s.events.push(e);
                    }
                },
                S {
                    groups,
                    selected: selected.map(Arc::from),
                    events: Vec::new(),
                },
            )
    }

    #[test]
    fn chips_show_each_repository_with_its_process_count() {
        let mut h = harness(None);
        h.run();
        assert!(h.query_by_label("Atropos (1)").is_some());
        assert!(h.query_by_label_contains("End everything").is_none());
        h.get_by_label("shop (2)").click();
        h.run();
        assert_eq!(
            h.state().events,
            vec![BarEvent::Pick(Some(Arc::from(r"D:\code\shop")))]
        );
    }

    #[test]
    fn a_picked_repository_offers_the_group_kill_and_unpicks_on_a_second_click() {
        let mut h = harness(Some(r"D:\code\shop"));
        h.run();
        h.get_by_label(&kill_all_text("shop")).click();
        h.run();
        h.get_by_label("shop (2)").click();
        h.run();
        assert_eq!(
            h.state().events,
            vec![
                BarEvent::KillAll(Arc::from(r"D:\code\shop")),
                BarEvent::Pick(None)
            ]
        );
    }

    #[test]
    fn no_repositories_says_so() {
        let mut h = harness(None);
        h.state_mut().groups.clear();
        h.run();
        assert!(
            h.query_by_label_contains("No process is running inside a git repository")
                .is_some()
        );
    }

    #[test]
    fn a_picked_repository_that_has_no_processes_left_is_unpicked() {
        let mut h = harness(Some(r"D:\code\gone"));
        h.run();
        assert_eq!(h.state().events.first(), Some(&BarEvent::Pick(None)));
    }
}
