//! System header: CPU and memory sparklines (120 samples) and the process count.

use atropos_core::collector::Frame;
use atropos_core::ring::Ring;
use eframe::egui::{self, Color32, Pos2, Rect, Stroke, vec2};

use crate::format;

const SPARK_SIZE: egui::Vec2 = vec2(120.0, 22.0);

/// Maps samples (0–100 %) to points across `rect`: oldest at the left, 100 % at the top.
/// The newest sample always sits at the right edge, so the line grows leftward at start-up.
#[must_use]
#[allow(clippy::cast_precision_loss)] // N and indices are tiny
pub fn sparkline_points<const N: usize>(ring: &Ring<N>, rect: Rect) -> Vec<Pos2> {
    let step = if N > 1 {
        rect.width() / (N - 1) as f32
    } else {
        0.0
    };
    let len = ring.len();
    ring.iter()
        .enumerate()
        .map(|(i, v)| {
            let x = rect.right() - (len - 1 - i) as f32 * step;
            let y = rect.bottom() - v.clamp(0.0, 100.0) / 100.0 * rect.height();
            Pos2::new(x.max(rect.left()), y)
        })
        .collect()
}

fn sparkline<const N: usize>(ui: &mut egui::Ui, ring: &Ring<N>, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(SPARK_SIZE, egui::Sense::hover());
    let p = ui.painter_at(rect);
    p.rect_filled(rect, 2.0, ui.visuals().extreme_bg_color);
    p.add(egui::Shape::line(
        sparkline_points(ring, rect.shrink(1.0)),
        Stroke::new(1.5_f32, color),
    ));
}

/// Draws the header for `frame`.
pub fn show(ui: &mut egui::Ui, frame: &Frame) {
    let stats = frame.snapshot.stats;
    ui.horizontal(|ui| {
        let cpu = frame.snapshot.total_cpu_pct();
        ui.label(format!("CPU {}%", format::pct(cpu)))
            .on_hover_text("Whole-machine CPU over the last 120 updates");
        sparkline(ui, &frame.cpu_history, Color32::from_rgb(255, 120, 90));
        ui.add_space(12.0);
        ui.label(format!(
            "Memory {} / {}",
            format::bytes(stats.used_ram),
            format::bytes(stats.total_ram)
        ))
        .on_hover_text("Physical memory in use over the last 120 updates");
        sparkline(ui, &frame.mem_history, Color32::from_rgb(120, 170, 255));
        ui.add_space(12.0);
        ui.label(format!("{} processes", frame.snapshot.procs.len()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[allow(clippy::float_cmp)]
    fn sparkline_maps_percent_to_height_newest_at_right() {
        let mut r = Ring::<5>::default();
        for v in [0.0, 50.0, 100.0] {
            r.push(v);
        }
        let rect = Rect::from_min_size(Pos2::new(10.0, 0.0), vec2(40.0, 20.0));
        let pts = sparkline_points(&r, rect);
        assert_eq!(pts.len(), 3);
        assert_eq!(pts[2], Pos2::new(50.0, 0.0)); // newest, 100 %: right edge, top
        assert_eq!(pts[1], Pos2::new(40.0, 10.0)); // 50 %: middle
        assert_eq!(pts[0], Pos2::new(30.0, 20.0)); // 0 %: bottom
    }

    #[test]
    fn sparkline_clamps_out_of_range() {
        let mut r = Ring::<2>::default();
        r.push(-5.0);
        r.push(250.0);
        let rect = Rect::from_min_size(Pos2::ZERO, vec2(10.0, 10.0));
        let pts = sparkline_points(&r, rect);
        assert!(pts.iter().all(|p| rect.contains(*p)));
    }
}
