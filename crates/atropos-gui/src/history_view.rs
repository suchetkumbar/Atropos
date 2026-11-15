//! The History window (spec Phase 6): what Atropos ended and when, newest first, with
//! **Restart** for entries whose launch record was captured. Environment values are never
//! shown here (only how many there are); neither is the command line, which can carry tokens.

use atropos_core::history::{Entry, Launch};
use atropos_core::platform::windows::{LocalTime, local_time};
use eframe::egui;

use crate::format;

/// The most entries the window lists.
pub const SHOWN: usize = 200;

/// Window state.
#[derive(Debug, Default)]
pub struct HistoryUi {
    pub open: bool,
    /// Oldest first, as read from the file.
    pub entries: Vec<Entry>,
}

/// Draws the window; returns the launch record the user asked to restart.
pub fn show(ctx: &egui::Context, st: &mut HistoryUi, today: Option<LocalTime>) -> Option<Launch> {
    let mut asked = None;
    let mut open = st.open;
    egui::Window::new("History")
        .open(&mut open)
        .collapsible(false)
        .default_width(560.0)
        .show(ctx, |ui| {
            if st.entries.is_empty() {
                ui.label("Nothing ended yet. Processes Atropos ends are listed here.");
                return;
            }
            egui::ScrollArea::vertical()
                .max_height(420.0)
                .show(ui, |ui| {
                    egui::Grid::new("history")
                        .num_columns(4)
                        .striped(true)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            for e in st.entries.iter().rev().take(SHOWN) {
                                let when = match (local_time(e.at), today) {
                                    (Some(t), Some(d)) => format::started(t, d),
                                    (Some(t), None) => format::started(t, t),
                                    (None, _) => "—".to_owned(),
                                };
                                ui.label(when);
                                let what = if e.tree {
                                    format!("{} ({}) and its children", e.name, e.pid)
                                } else {
                                    format!("{} ({})", e.name, e.pid)
                                };
                                ui.vertical(|ui| {
                                    ui.label(what);
                                    if let Some(l) = &e.launch {
                                        let n = l.env.len();
                                        ui.label(
                                        egui::RichText::new(format!(
                                            "in {} · {n} environment variable{} (values not shown)",
                                            l.cwd,
                                            if n == 1 { "" } else { "s" }
                                        ))
                                        .weak()
                                        .small(),
                                    );
                                    }
                                });
                                ui.label(match e.outcome.as_str() {
                                    "exited" => "closed",
                                    _ => "forced",
                                });
                                match &e.launch {
                                    Some(l) => {
                                        if ui.button("Restart").on_hover_text(&l.exe).clicked() {
                                            asked = Some(l.clone());
                                        }
                                    }
                                    None => {
                                        ui.label("—").on_hover_text(
                                            "No Restart: how it was started could not be read",
                                        );
                                    }
                                }
                                ui.end_row();
                            }
                        });
                });
        });
    st.open = open;
    asked
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    fn entry(pid: u32, name: &str, launch: bool) -> Entry {
        Entry {
            v: 1,
            at: 133_000_000_000_000_000 + u64::from(pid),
            pid,
            create_time: u64::from(pid),
            name: name.into(),
            tree: pid == 2,
            outcome: "terminated".into(),
            launch: launch.then(|| Launch {
                exe: r"C:\tools\node.exe".into(),
                cmdline: "node server.js --token=abc123".into(),
                cwd: r"D:\code\shop\".into(),
                env: vec![("API_TOKEN".into(), "s3cr3t".into())],
                exe_len: 1,
                exe_modified: 1,
            }),
        }
    }

    struct S {
        ui: HistoryUi,
        asked: Vec<Launch>,
    }

    fn harness(entries: Vec<Entry>) -> Harness<'static, S> {
        Harness::builder()
            .with_size(egui::vec2(800.0, 500.0))
            .build_state(
                |ctx, s: &mut S| {
                    if let Some(l) = show(ctx, &mut s.ui, None) {
                        s.asked.push(l);
                    }
                },
                S {
                    ui: HistoryUi {
                        open: true,
                        entries,
                    },
                    asked: Vec::new(),
                },
            )
    }

    #[test]
    fn entries_are_listed_and_restart_returns_the_launch_record() {
        let mut h = harness(vec![
            entry(1, "node.exe", true),
            entry(2, "vite.exe", false),
        ]);
        h.run();
        assert!(h.query_by_label_contains("node.exe (1)").is_some());
        assert!(h.query_by_label_contains("vite.exe (2)").is_some());
        h.get_by_label("Restart").click();
        h.run();
        assert_eq!(h.state().asked.len(), 1);
        assert_eq!(h.state().asked[0].cwd, r"D:\code\shop\");
    }

    #[test]
    fn secrets_are_never_shown() {
        let mut h = harness(vec![entry(1, "node.exe", true)]);
        h.run();
        assert!(h.query_by_label_contains("s3cr3t").is_none());
        assert!(h.query_by_label_contains("abc123").is_none());
        assert!(
            h.query_by_label_contains("1 environment variable")
                .is_some()
        );
    }

    #[test]
    fn an_empty_history_says_so() {
        let mut h = harness(Vec::new());
        h.run();
        assert!(h.query_by_label_contains("Nothing ended yet").is_some());
    }
}
