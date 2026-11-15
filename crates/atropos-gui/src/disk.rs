//! Disk view (spec Phase 5): processes doing disk I/O, busiest first, with one bar each (read
//! and write stacked). The rates are the tier-0 read/write counters; nothing extra is read.

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::snapshot::Proc;
use eframe::egui::{self, Color32, Sense};

use crate::format;
use crate::view::{Row, display_name};

const ROW_HEIGHT: f32 = 22.0;
const NAME_WIDTH: f32 = 260.0;
const TEXT_WIDTH: f32 = 190.0;
const READ: Color32 = Color32::from_rgb(64, 140, 214);
const WRITE: Color32 = Color32::from_rgb(222, 140, 48);

/// Read + write bytes per second; `None` before the second sample.
#[must_use]
pub fn total(p: &Proc) -> Option<f32> {
    match (p.read_rate, p.write_rate) {
        (None, None) => None,
        (r, w) => Some(r.unwrap_or(0.0) + w.unwrap_or(0.0)),
    }
}

/// Whether the process did any disk I/O since the last sample.
#[must_use]
pub fn active(p: &Proc) -> bool {
    total(p).is_some_and(|t| t > 0.0)
}

/// The rows doing I/O, busiest first (ties by PID).
#[must_use]
pub fn ranked(frame: &Frame, rows: &[Row]) -> Vec<Row> {
    let mut out: Vec<Row> = rows
        .iter()
        .filter(|r| active(&frame.snapshot.procs[r.idx]))
        .copied()
        .collect();
    let t = |r: &Row| total(&frame.snapshot.procs[r.idx]).unwrap_or(0.0);
    out.sort_by(|a, b| t(b).total_cmp(&t(a)).then(a.key.pid.cmp(&b.key.pid)));
    out
}

/// Draws the ranked list; returns the process the user clicked.
pub fn show(
    ui: &mut egui::Ui,
    frame: &Frame,
    rows: &[Row],
    selected: Option<ProcKey>,
) -> Option<ProcKey> {
    let list = ranked(frame, rows);
    ui.horizontal(|ui| {
        ui.colored_label(READ, "■ read");
        ui.colored_label(WRITE, "■ write");
        ui.label(
            egui::RichText::new("bytes per second since the last update, all I/O Windows counts")
                .weak()
                .small(),
        );
    });
    if list.is_empty() {
        ui.label("No disk activity right now.");
        return None;
    }
    let max = list
        .first()
        .map_or(1.0, |r| total(&frame.snapshot.procs[r.idx]).unwrap_or(1.0))
        .max(1.0);
    let mut clicked = None;
    egui::ScrollArea::vertical().show_rows(ui, ROW_HEIGHT, list.len(), |ui, range| {
        for row in &list[range] {
            let p = &frame.snapshot.procs[row.idx];
            let (read, write) = (p.read_rate.unwrap_or(0.0), p.write_rate.unwrap_or(0.0));
            let (rect, resp) = ui
                .allocate_exact_size(egui::vec2(ui.available_width(), ROW_HEIGHT), Sense::click());
            let painter = ui.painter_at(rect);
            if selected == Some(row.key) {
                painter.rect_filled(rect, 2.0, ui.visuals().selection.bg_fill);
            } else if resp.hovered() {
                painter.rect_filled(rect, 2.0, ui.visuals().widgets.hovered.weak_bg_fill);
            }
            let text = ui.visuals().text_color();
            let font = egui::FontId::proportional(13.0);
            let mid = rect.left_center();
            painter.text(
                mid + egui::vec2(4.0, 0.0),
                egui::Align2::LEFT_CENTER,
                format!("{} ({})", display_name(frame, p), p.raw.key.pid),
                font.clone(),
                text,
            );
            let bar_x = rect.left() + NAME_WIDTH;
            let bar_w = (rect.width() - NAME_WIDTH - TEXT_WIDTH).max(20.0);
            let h = ROW_HEIGHT - 8.0;
            let y = rect.center().y - h / 2.0;
            let rw = bar_w * read / max;
            let ww = bar_w * write / max;
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(bar_x, y), egui::vec2(rw, h)),
                0.0,
                READ,
            );
            painter.rect_filled(
                egui::Rect::from_min_size(egui::pos2(bar_x + rw, y), egui::vec2(ww, h)),
                0.0,
                WRITE,
            );
            painter.text(
                egui::pos2(bar_x + bar_w + 8.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                format!(
                    "R {} · W {}",
                    format::rate(p.read_rate),
                    format::rate(p.write_rate)
                ),
                font,
                text,
            );
            let resp = resp.on_hover_text(format!(
                "{}\nRead {} · Write {}",
                display_name(frame, p),
                format::rate(p.read_rate),
                format::rate(p.write_rate)
            ));
            if resp.clicked() {
                clicked = Some(row.key);
            }
        }
    });
    clicked
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, k};
    use crate::view::{Scope, ViewOpts, rows};
    use atropos_core::filter::{Filter, FilterMatcher};
    use egui_kittest::Harness;

    /// [`frame`] with disk rates (read, write) per PID; the others idle.
    fn busy(rates: &[(u32, f32, f32)]) -> Frame {
        let mut f = frame();
        for p in &mut f.snapshot.procs {
            let (r, w) = rates
                .iter()
                .find(|(pid, ..)| *pid == p.raw.key.pid)
                .map_or((0.0, 0.0), |&(_, r, w)| (r, w));
            p.read_rate = Some(r);
            p.write_rate = Some(w);
        }
        f
    }

    fn disk_rows(f: &Frame) -> Vec<Row> {
        let opts = ViewOpts {
            scope: Scope::Disk,
            ..ViewOpts::default()
        };
        rows(
            f,
            &Filter::default(),
            "",
            &opts,
            &mut FilterMatcher::default(),
        )
    }

    #[test]
    fn only_processes_doing_io_are_listed_busiest_first() {
        let f = busy(&[(3, 100.0, 0.0), (4, 10.0, 500.0), (9, 0.0, 100.0)]);
        let pids: Vec<u32> = ranked(&f, &disk_rows(&f))
            .iter()
            .map(|r| r.key.pid)
            .collect();
        // 4: 510 B/s; 3 and 9 tie at 100 B/s → by PID.
        assert_eq!(pids, vec![4, 3, 9]);
    }

    #[test]
    fn the_disk_scope_hides_idle_processes_and_first_samples() {
        let mut f = busy(&[(2, 1.0, 0.0)]);
        f.snapshot.procs[0].read_rate = None;
        f.snapshot.procs[0].write_rate = None;
        let r = disk_rows(&f);
        assert_eq!(r.iter().map(|r| r.key).collect::<Vec<_>>(), vec![k(2)]);
    }

    struct S {
        frame: Frame,
        rows: Vec<Row>,
        selected: Option<ProcKey>,
    }

    fn harness(f: Frame) -> Harness<'static, S> {
        let rows = disk_rows(&f);
        Harness::builder()
            .with_size(egui::vec2(900.0, 300.0))
            .build_ui_state(
                |ui, s: &mut S| {
                    if let Some(k) = show(ui, &s.frame, &s.rows, s.selected) {
                        s.selected = Some(k);
                    }
                },
                S {
                    frame: f,
                    rows,
                    selected: None,
                },
            )
    }

    #[test]
    fn clicking_a_line_selects_its_process() {
        let mut h = harness(busy(&[(3, 100.0, 0.0), (4, 10.0, 500.0)]));
        h.run();
        // Second line (Code.exe, PID 3): below the legend and the first line.
        let legend = 24.0;
        let at = egui::pos2(100.0, legend + ROW_HEIGHT * 1.5);
        let input = h.input_mut();
        input.events.push(egui::Event::PointerMoved(at));
        for pressed in [true, false] {
            input.events.push(egui::Event::PointerButton {
                pos: at,
                button: egui::PointerButton::Primary,
                pressed,
                modifiers: egui::Modifiers::NONE,
            });
        }
        h.run();
        assert_eq!(h.state().selected, Some(k(3)));
    }

    #[test]
    fn no_activity_says_so() {
        use egui_kittest::kittest::Queryable as _;
        let mut h = harness(busy(&[]));
        h.run();
        assert!(h.query_by_label("No disk activity right now.").is_some());
    }
}
