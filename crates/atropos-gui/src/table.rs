//! The process table (flat or tree). Rendering only: it reports clicks as [`Event`]s and the
//! app decides what they mean. Rows are virtualized by `egui_extras`.

use atropos_core::collector::Frame;
use atropos_core::platform::windows::LocalTime;
use atropos_core::snapshot::Proc;
use atropos_core::{Field, ProcKey};
use eframe::egui::{self, Align, Layout, RichText};
use egui_extras::{Column, TableBuilder};

use crate::actions::{Act, Actions};
use crate::format;
use crate::view::{Row, Sort, SortKey};

const ROW_HEIGHT: f32 = 20.0;
const INDENT: f32 = 14.0;
/// Shown in place of a value the OS would not reveal.
pub const LOCK: &str = "🔒";
pub const RESTRICTED_TIP: &str = "Access denied: this process runs as another user or is \
    protected, so Windows does not let your account read this value.";

/// What the user did in the table this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Event {
    Select(ProcKey),
    Sort(SortKey),
    /// Expand or collapse a tree node.
    Toggle(ProcKey),
    /// Context-menu action.
    Act(ProcKey, Act),
}

/// Pause marker for suspended processes.
pub const PAUSED: &str = "⏸";

/// Columns, in order: title, sort key, tooltip.
pub const COLUMNS: [(&str, SortKey, &str); 10] = [
    (
        "Name",
        SortKey::Name,
        "What the process is: a smart label such as \"Next.js dev · shop\" when Atropos \
         recognises it, else the executable name",
    ),
    (
        "PID",
        SortKey::Pid,
        "Process ID (reused by Windows after a process exits)",
    ),
    (
        "CPU %",
        SortKey::Cpu,
        "Share of the whole machine's CPU since the last update, like Task Manager. \
         Turn on per-core to make 100 % mean one full core.",
    ),
    (
        "Memory",
        SortKey::Memory,
        "Private working set: memory only this process uses that is in RAM now \
         (Task Manager's Memory column)",
    ),
    ("Threads", SortKey::Threads, "Number of threads"),
    ("Handles", SortKey::Handles, "Open kernel object handles"),
    (
        "Disk R/s",
        SortKey::DiskRead,
        "Bytes read per second, all I/O Windows counts for the process",
    ),
    (
        "Disk W/s",
        SortKey::DiskWrite,
        "Bytes written per second, all I/O Windows counts for the process",
    ),
    ("User", SortKey::User, "Account the process runs as"),
    ("Started", SortKey::Started, "Start time, local"),
];

/// Starting widths: Name, then PID … User; Started takes the remaining space.
const NAME_WIDTH: f32 = 240.0;
const WIDTHS: [f32; 8] = [60.0, 56.0, 76.0, 56.0, 64.0, 72.0, 72.0, 96.0];

/// Display inputs that are not part of the frame.
#[derive(Debug, Clone, Copy)]
pub struct Look {
    pub selected: Option<ProcKey>,
    pub sort: Sort,
    /// CPU % relative to one core (htop-style) instead of the whole machine.
    pub per_core: bool,
    pub today: Option<LocalTime>,
}

fn cpu_text(frame: &Frame, p: &Proc, row: &Row, per_core: bool) -> String {
    let v = if row.collapsed {
        Some(p.rollup.cpu_pct)
    } else {
        p.cpu_pct
    };
    #[allow(clippy::cast_precision_loss)] // logical CPU count
    let scale = if per_core {
        frame.snapshot.stats.logical_cpus.max(1) as f32
    } else {
        1.0
    };
    format::pct(v.map(|v| v * scale))
}

fn right(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
    ui.with_layout(Layout::right_to_left(Align::Center), |ui| ui.label(text))
        .inner
}

/// Draws the table; returns the events of this frame.
pub fn show(
    ui: &mut egui::Ui,
    frame: &Frame,
    rows: &[Row],
    look: Look,
    actions: &Actions,
) -> Vec<Event> {
    let mut events = Vec::new();
    // Selectable labels would swallow the click meant for the row.
    ui.style_mut().interaction.selectable_labels = false;
    let mut tb = TableBuilder::new(ui)
        .striped(true)
        .resizable(true)
        .sense(egui::Sense::click())
        .column(Column::initial(NAME_WIDTH).at_least(120.0).clip(true));
    for w in WIDTHS {
        tb = tb.column(Column::initial(w).at_least(40.0).clip(true));
    }
    tb = tb.column(Column::remainder().at_least(80.0).clip(true));
    tb.header(ROW_HEIGHT, |mut header| {
        for (title, key, tip) in COLUMNS {
            header.col(|ui| {
                let arrow = match look.sort {
                    Sort {
                        key: k,
                        descending: true,
                    } if k == key => " ⏷",
                    Sort {
                        key: k,
                        descending: false,
                    } if k == key => " ⏶",
                    _ => "",
                };
                let b = egui::Button::new(RichText::new(format!("{title}{arrow}")).strong())
                    .frame(false);
                if ui.add(b).on_hover_text(tip).clicked() {
                    events.push(Event::Sort(key));
                }
            });
        }
    })
    .body(|body| {
        body.rows(ROW_HEIGHT, rows.len(), |mut tr| {
            let row = rows[tr.index()];
            tr.set_selected(look.selected == Some(row.key));
            row_cells(&mut tr, frame, &row, look, actions, &mut events);
            let resp = tr.response();
            if resp.clicked() {
                events.push(Event::Select(row.key));
            }
            resp.context_menu(|ui| {
                let suspended = frame.snapshot.get(row.key).is_some_and(|p| p.raw.suspended);
                let items = [
                    ("End process", "Del", Act::Kill),
                    ("End process tree", "Shift+Del", Act::KillTree),
                    if suspended {
                        ("Resume", "Ctrl+P", Act::Resume)
                    } else {
                        ("Suspend", "Ctrl+P", Act::Suspend)
                    },
                ];
                for (text, keys, act) in items {
                    if ui
                        .add(egui::Button::new(text).shortcut_text(keys))
                        .clicked()
                    {
                        events.push(Event::Select(row.key));
                        events.push(Event::Act(row.key, act));
                        ui.close();
                    }
                }
            });
        });
    });
    events
}

/// Cell text of one row, in column order. The User cell says `Restricted` when access was
/// denied (the table draws a lock there instead).
fn cell_texts(frame: &Frame, row: &Row, per_core: bool, today: Option<LocalTime>) -> [String; 10] {
    let p = &frame.snapshot.procs[row.idx];
    let ws = if row.collapsed {
        p.rollup.private_ws
    } else {
        p.raw.private_ws
    };
    let user = match frame.statics.get(&row.key).map(|s| &s.user) {
        Some(Field::Ok(u)) => u.to_string(),
        Some(Field::Restricted) => "Restricted".to_owned(),
        _ => "—".to_owned(),
    };
    let started = match (
        atropos_core::platform::windows::local_time(p.raw.key.create_time),
        today,
    ) {
        (Some(t), Some(today)) if p.raw.key.create_time > 0 => format::started(t, today),
        _ => "—".to_owned(),
    };
    [
        crate::view::display_name(frame, p).to_owned(),
        p.raw.key.pid.to_string(),
        cpu_text(frame, p, row, per_core),
        format::bytes(ws),
        p.raw.threads.to_string(),
        p.raw.handles.to_string(),
        format::rate(p.read_rate),
        format::rate(p.write_rate),
        user,
        started,
    ]
}

/// The row as tab-separated text (`Ctrl+C`), in column order; ready to paste into a sheet.
#[must_use]
pub fn row_text(frame: &Frame, row: &Row, per_core: bool, today: Option<LocalTime>) -> String {
    cell_texts(frame, row, per_core, today).join("\t")
}

/// The ten cells of one row.
fn row_cells(
    tr: &mut egui_extras::TableRow<'_, '_>,
    frame: &Frame,
    row: &Row,
    look: Look,
    actions: &Actions,
    events: &mut Vec<Event>,
) {
    let st = frame.statics.get(&row.key);
    let suspended = frame.snapshot.procs[row.idx].raw.suspended;
    let [name, numbers @ .., user, started] = cell_texts(frame, row, look.per_core, look.today);
    tr.col(|ui| {
        ui.add_space(f32::from(row.depth) * INDENT);
        if row.has_children {
            let glyph = if row.collapsed { "⏵" } else { "⏷" };
            if ui.add(egui::Button::new(glyph).frame(false)).clicked() {
                events.push(Event::Toggle(row.key));
            }
        }
        let count = actions.suspend_count(row.key);
        let shown = match (suspended, count) {
            (true, n) if n > 1 => format!("{PAUSED} {name} ×{n}"),
            (true, _) => format!("{PAUSED} {name}"),
            _ => name,
        };
        let name = ui.label(shown);
        // A labelled row still says which executable it is.
        let image = &frame.snapshot.procs[row.idx].raw.image;
        let labelled = frame
            .context
            .get(&row.key)
            .is_some_and(|c| c.label.is_some());
        match (labelled, st.and_then(|s| s.exe_path.ok())) {
            (true, Some(path)) => name.on_hover_text(format!("{image}\n{path}")),
            (true, None) => name.on_hover_text(&**image),
            (false, Some(path)) => name.on_hover_text(&**path),
            (false, None) => name,
        };
        if let Some(status) = actions.status().get(&row.key) {
            ui.label(RichText::new(status).weak().italics());
        }
    });
    for text in numbers {
        tr.col(|ui| {
            right(ui, text);
        });
    }
    tr.col(|ui| {
        if matches!(st.map(|s| &s.user), Some(Field::Restricted)) {
            ui.label(LOCK).on_hover_text(RESTRICTED_TIP);
        } else {
            ui.label(user);
        }
    });
    tr.col(|ui| {
        right(ui, started);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, k};
    use crate::view::{ViewOpts, rows};
    use atropos_core::filter::{Filter, FilterMatcher};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct State {
        frame: Frame,
        opts: ViewOpts,
        selected: Option<ProcKey>,
        rows: Vec<Row>,
        actions: Actions,
        acts: Vec<(ProcKey, Act)>,
    }

    impl State {
        fn new() -> Self {
            let mut s = Self {
                frame: frame(),
                opts: ViewOpts::default(),
                selected: None,
                rows: Vec::new(),
                actions: Actions::new(None),
                acts: Vec::new(),
            };
            s.recompute();
            s
        }

        fn recompute(&mut self) {
            self.rows = rows(
                &self.frame,
                &Filter::default(),
                "josé",
                &self.opts,
                &mut FilterMatcher::default(),
            );
        }
    }

    fn harness() -> Harness<'static, State> {
        Harness::builder()
            .with_size(egui::vec2(1100.0, 400.0))
            .build_ui_state(
                |ui, s: &mut State| {
                    let look = Look {
                        selected: s.selected,
                        sort: s.opts.sort,
                        per_core: false,
                        today: None,
                    };
                    for e in show(ui, &s.frame, &s.rows, look, &s.actions) {
                        match e {
                            Event::Select(key) => s.selected = Some(key),
                            Event::Sort(key) => {
                                s.opts.sort = Sort {
                                    key,
                                    descending: false,
                                };
                                s.recompute();
                            }
                            Event::Toggle(key) => {
                                if !s.opts.collapsed.remove(&key) {
                                    s.opts.collapsed.insert(key);
                                }
                                s.recompute();
                            }
                            Event::Act(key, act) => s.acts.push((key, act)),
                        }
                    }
                },
                State::new(),
            )
    }

    #[test]
    fn click_selects_and_resort_keeps_the_selection() {
        let mut h = harness();
        h.run();
        h.get_by_label("Code.exe").click();
        h.run();
        assert_eq!(h.state().selected, Some(k(3)));
        let before = h.state().rows.iter().position(|r| r.key == k(3));
        h.get_by_label("Name").click();
        h.run();
        let after = h.state().rows.iter().position(|r| r.key == k(3));
        assert_ne!(before, after, "the sort must actually move the row");
        assert_eq!(h.state().selected, Some(k(3)));
        assert_eq!(h.state().opts.sort.key, SortKey::Name);
    }

    #[test]
    fn row_text_is_the_visible_cells_tab_separated() {
        let f = frame();
        let row = Row {
            idx: f.snapshot.index[&k(2)],
            key: k(2),
            depth: 0,
            has_children: false,
            collapsed: false,
        };
        assert_eq!(
            row_text(&f, &row, false, None),
            "node.exe\t2\t30.0\t0 KB\t3\t0\t0 B/s\t0 B/s\tjosé\t—"
        );
        let svc = Row {
            idx: f.snapshot.index[&k(9)],
            key: k(9),
            ..row
        };
        assert!(row_text(&f, &svc, false, None).contains("\tRestricted\t"));
    }

    #[test]
    fn context_menu_starts_actions() {
        let mut h = harness();
        h.run();
        h.get_by_label("Code.exe").click_secondary();
        h.run();
        h.get_by_label_contains("End process tree").click();
        h.run();
        assert_eq!(h.state().acts, vec![(k(3), Act::KillTree)]);
        assert_eq!(h.state().selected, Some(k(3)));
    }

    #[test]
    fn suspended_rows_show_a_pause_marker_and_status() {
        let mut h = harness();
        let i = h.state().frame.snapshot.index[&k(3)];
        h.state_mut().frame.snapshot.procs[i].raw.suspended = true;
        h.state_mut().actions.set_status(k(2), "Closing… 3s");
        h.run();
        assert!(h.query_by_label("⏸ Code.exe").is_some());
        assert!(h.query_by_label("Closing… 3s").is_some());
    }

    #[test]
    fn restricted_cell_shows_lock() {
        let mut h = harness();
        h.run();
        assert_eq!(h.query_all_by_label(LOCK).count(), 1);
        assert_eq!(h.query_all_by_label("josé").count(), 4);
    }

    #[test]
    fn tree_expand_collapse() {
        let mut h = harness();
        h.state_mut().opts.tree = true;
        h.state_mut().recompute();
        h.run();
        assert_eq!(h.query_all_by_label("node.exe").count(), 2);
        // Collapse explorer (the first expander): its whole subtree disappears.
        h.get_all_by_label("⏷").next().unwrap().click();
        h.run();
        assert!(h.state().opts.collapsed.contains(&k(1)));
        assert_eq!(h.query_all_by_label("node.exe").count(), 0);
        h.get_by_label("⏵").click();
        h.run();
        assert_eq!(h.query_all_by_label("node.exe").count(), 2);
    }

    #[test]
    fn name_column_shows_the_smart_label() {
        let mut h = harness();
        h.state_mut().frame = crate::test_frame::frame_with_context();
        h.state_mut().recompute();
        h.run();
        assert!(h.query_by_label("Next.js dev · shop").is_some());
        assert!(h.query_by_label("VS Code").is_some());
        // 4 has no label and keeps its image name; 2's image moved to the tooltip.
        assert_eq!(h.query_all_by_label("node.exe").count(), 1);
        let s = h.state();
        let row = s.rows.iter().find(|r| r.key == k(2)).unwrap();
        assert!(row_text(&s.frame, row, false, None).starts_with("Next.js dev · shop\t2\t"));
    }
}
