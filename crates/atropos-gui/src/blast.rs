//! Blast Radius (spec Phase 5): the process tree as a squarified treemap. A cell is sized by
//! its subtree's memory and coloured by CPU; a parent with room shows its children inside, under
//! a header strip. Rectangles hold still until a size moves by more than 10 % (hysteresis).
//!
//! Selection is by `ProcKey`, so it survives re-layouts. Kill keys act on the selection through
//! the app's usual `Actions` flow; this module never kills anything.

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::treemap::{self, Hysteresis, Item, MIN_AREA, Rect, Slot};
use eframe::egui::{self, Color32, Pos2, Sense, Stroke, StrokeKind};

use crate::format;
use crate::view::display_name;

/// Height of a group's header strip (its name), px.
pub const HEADER: f32 = 16.0;
/// Inset of a group's children, px.
pub const PAD: f32 = 2.0;
/// A group smaller than this (w, h) is drawn as one leaf instead of showing its children.
pub const MIN_GROUP: (f32, f32) = (48.0, 36.0);
const MAX_DEPTH: u16 = 16;

/// What a cell stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A process whose children are drawn inside it.
    Group,
    /// A process drawn as one block (its subtree, if any, is too small to open up).
    Leaf,
    /// Inside a group: the parent's own memory, next to its children.
    Own,
    /// `count` processes too small to draw. Never selectable.
    Other { count: usize },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct BlastCell {
    /// `None` for [`Kind::Other`].
    pub key: Option<ProcKey>,
    pub kind: Kind,
    pub rect: Rect,
    pub depth: u16,
}

/// What a treemap item is while laying out: a subtree or a parent's own share.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Node {
    Sub(usize),
    Own(usize),
}

/// What sizes and colours the cells.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Measure {
    /// Size by private memory, colour by CPU.
    #[default]
    Memory,
    /// Size by dedicated GPU memory, colour by GPU utilisation.
    Gpu,
}

/// Per process (by snapshot index): own and subtree size, own and subtree heat (0..=1).
#[derive(Debug, Clone, Default)]
pub struct Measures {
    pub own: Vec<f64>,
    pub sub: Vec<f64>,
    pub own_heat: Vec<f32>,
    pub sub_heat: Vec<f32>,
}

/// The sizes and heats of `frame` for `m`. Memory uses the snapshot rollups; GPU figures are
/// rolled up here (sizes summed, heat the busiest process of the subtree). Without GPU
/// figures every GPU size is 0, so nothing is drawn.
#[must_use]
#[allow(clippy::cast_precision_loss)] // byte counts as sizes, CPU count as a scale
pub fn measures(frame: &Frame, m: Measure) -> Measures {
    let snap = &frame.snapshot;
    let cpus = snap.stats.logical_cpus.max(1) as f32;
    match m {
        Measure::Memory => Measures {
            own: snap.procs.iter().map(|p| p.raw.private_ws as f64).collect(),
            sub: snap
                .procs
                .iter()
                .map(|p| p.rollup.private_ws as f64)
                .collect(),
            own_heat: snap
                .procs
                .iter()
                .map(|p| p.cpu_pct.unwrap_or(0.0) * cpus / 100.0)
                .collect(),
            sub_heat: snap
                .procs
                .iter()
                .map(|p| p.rollup.cpu_pct * cpus / 100.0)
                .collect(),
        },
        Measure::Gpu => {
            let usage = |p: &atropos_core::snapshot::Proc| {
                frame
                    .gpu
                    .as_ref()
                    .and_then(|g| g.get(&p.raw.key))
                    .copied()
                    .unwrap_or_default()
            };
            let own: Vec<f64> = snap
                .procs
                .iter()
                .map(|p| usage(p).dedicated as f64)
                .collect();
            let own_heat: Vec<f32> = snap
                .procs
                .iter()
                .map(|p| usage(p).util_pct / 100.0)
                .collect();
            let (mut sub, mut sub_heat) = (own.clone(), own_heat.clone());
            // Children before parents: reverse of a pre-order walk from the roots.
            let mut order = Vec::with_capacity(snap.procs.len());
            let mut stack: Vec<usize> = snap.roots.clone();
            while let Some(i) = stack.pop() {
                order.push(i);
                stack.extend(&snap.children[i]);
            }
            for &i in order.iter().rev() {
                for &c in &snap.children[i] {
                    sub[i] += sub[c];
                    sub_heat[i] = sub_heat[i].max(sub_heat[c]);
                }
            }
            Measures {
                own,
                sub,
                own_heat,
                sub_heat,
            }
        }
    }
}

/// Lays the tree out in `rect` by memory: the whole machine, or the subtree of `zoom`.
#[cfg(test)]
#[must_use]
pub fn cells(frame: &Frame, zoom: Option<ProcKey>, rect: Rect) -> Vec<BlastCell> {
    cells_by(frame, &measures(frame, Measure::Memory), zoom, rect)
}

/// Lays the tree out in `rect` with the sizes in `m`.
#[must_use]
pub fn cells_by(frame: &Frame, m: &Measures, zoom: Option<ProcKey>, rect: Rect) -> Vec<BlastCell> {
    let snap = &frame.snapshot;
    let top: Vec<Item<Node>> = match zoom.and_then(|k| snap.index.get(&k)) {
        Some(&i) => vec![Item {
            key: Node::Sub(i),
            size: m.sub[i],
        }],
        None => snap
            .roots
            .iter()
            .map(|&i| Item {
                key: Node::Sub(i),
                size: m.sub[i],
            })
            .collect(),
    };
    let mut out = Vec::new();
    place(frame, m, &top, rect, 0, &mut out);
    out
}

fn place(
    frame: &Frame,
    m: &Measures,
    items: &[Item<Node>],
    rect: Rect,
    depth: u16,
    out: &mut Vec<BlastCell>,
) {
    let snap = &frame.snapshot;
    for c in treemap::layout(items, rect) {
        let r = c.rect;
        let (i, kind) = match c.slot {
            Slot::Other { count } => {
                out.push(BlastCell {
                    key: None,
                    kind: Kind::Other { count },
                    rect: r,
                    depth,
                });
                continue;
            }
            Slot::Item(Node::Own(i)) => (i, Kind::Own),
            Slot::Item(Node::Sub(i)) => {
                let roomy = r.w >= MIN_GROUP.0 && r.h >= MIN_GROUP.1 && depth < MAX_DEPTH;
                if roomy && !snap.children[i].is_empty() {
                    (i, Kind::Group)
                } else {
                    (i, Kind::Leaf)
                }
            }
        };
        out.push(BlastCell {
            key: Some(snap.procs[i].raw.key),
            kind,
            rect: r,
            depth,
        });
        if kind == Kind::Group {
            let inner = Rect::new(r.x + PAD, r.y + HEADER, r.w - 2.0 * PAD, r.h - HEADER - PAD);
            let mut inside = vec![Item {
                key: Node::Own(i),
                size: m.own[i],
            }];
            inside.extend(snap.children[i].iter().map(|&j| Item {
                key: Node::Sub(j),
                size: m.sub[j],
            }));
            place(frame, m, &inside, inner, depth + 1, out);
        }
    }
}

/// The innermost cell under (`x`, `y`). Children come after their group, so the last hit wins.
#[must_use]
pub fn hit(cells: &[BlastCell], x: f32, y: f32) -> Option<&BlastCell> {
    cells.iter().rev().find(|c| {
        let r = c.rect;
        x >= r.x && x < r.x + r.w && y >= r.y && y < r.y + r.h
    })
}

/// The breadcrumb for `zoom`: its ancestors and itself, outermost first.
#[must_use]
pub fn trail(frame: &Frame, zoom: Option<ProcKey>) -> Vec<ProcKey> {
    let snap = &frame.snapshot;
    let mut out = Vec::new();
    let mut at = zoom.and_then(|k| snap.index.get(&k).copied());
    while let Some(i) = at {
        out.push(snap.procs[i].raw.key);
        at = snap.procs[i].parent;
    }
    out.reverse();
    out
}

/// Whether double-clicking `key` can zoom in: it has children.
fn zoomable(frame: &Frame, key: ProcKey) -> bool {
    frame
        .snapshot
        .index
        .get(&key)
        .is_some_and(|&i| !frame.snapshot.children[i].is_empty())
}

/// Treemap state kept across frames: the zoom and the last layout.
#[derive(Debug, Default)]
pub struct BlastState {
    pub zoom: Option<ProcKey>,
    pub measure: Measure,
    /// This frame's sizes and heats (colours update every frame, the layout only on change).
    m: Measures,
    /// The measure of the current layout.
    laid_by: Measure,
    hyst: Hysteresis<ProcKey>,
    /// Layout inputs and result: (rect, zoom, cells).
    laid: Option<(Rect, Option<ProcKey>, Vec<BlastCell>)>,
}

impl BlastState {
    /// The cells for this frame: the previous layout unless the area, the zoom, or a size (by
    /// more than 10 %) changed. A zoom target that exited falls back to the whole machine.
    #[cfg(test)]
    pub fn cells(&mut self, frame: &Frame, rect: Rect) -> &[BlastCell] {
        self.update(frame, rect);
        self.laid()
    }

    /// The current layout.
    #[must_use]
    pub fn laid(&self) -> &[BlastCell] {
        self.laid.as_ref().map_or(&[], |(_, _, c)| c.as_slice())
    }

    /// Recomputes this frame's measures and, when needed, the layout (see [`cells`](Self::cells)).
    pub fn update(&mut self, frame: &Frame, rect: Rect) {
        if self.zoom.is_some_and(|k| frame.snapshot.get(k).is_none()) {
            self.zoom = None;
        }
        let same = self.laid_by == self.measure
            && self
                .laid
                .as_ref()
                .is_some_and(|(r, z, _)| *r == rect && *z == self.zoom);
        if !same {
            self.hyst.reset();
        }
        self.m = measures(frame, self.measure);
        let sizes: Vec<(ProcKey, f64)> = frame
            .snapshot
            .procs
            .iter()
            .zip(&self.m.sub)
            .map(|(p, s)| (p.raw.key, *s))
            .collect();
        let total: f64 = frame.snapshot.roots.iter().map(|&i| self.m.sub[i]).sum();
        let area = f64::from(rect.w) * f64::from(rect.h);
        let ignore_below = if area > 0.0 {
            total / area * MIN_AREA
        } else {
            0.0
        };
        if self.hyst.update(&sizes, ignore_below) || self.laid.is_none() {
            self.laid = Some((rect, self.zoom, cells_by(frame, &self.m, self.zoom, rect)));
            self.laid_by = self.measure;
        }
    }

    /// Heat (0..=1) of a cell this frame: own figures for a parent's own share, else the subtree.
    fn heat_of(&self, frame: &Frame, c: &BlastCell) -> f32 {
        let Some(&i) = c.key.and_then(|k| frame.snapshot.index.get(&k)) else {
            return 0.0;
        };
        let v = if c.kind == Kind::Own {
            self.m.own_heat.get(i)
        } else {
            self.m.sub_heat.get(i)
        };
        v.copied().unwrap_or(0.0)
    }
}

/// What the user did in the treemap this frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlastEvent {
    Select(ProcKey),
    /// Show this subtree (`None` = the whole machine).
    Zoom(Option<ProcKey>),
}

/// Cell fill for a CPU share `t` in 0..=1 (of one core): calm blue to hot red.
#[must_use]
pub fn heat(t: f32) -> Color32 {
    let t = if t.is_finite() {
        t.clamp(0.0, 1.0)
    } else {
        0.0
    };
    let lerp = |a: u8, b: u8| {
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)] // 0..=255
        let v = (f32::from(a) + (f32::from(b) - f32::from(a)) * t).round() as u8;
        v
    };
    Color32::from_rgb(lerp(52, 214), lerp(96, 64), lerp(128, 44))
}

fn to_egui(r: Rect) -> egui::Rect {
    egui::Rect::from_min_size(Pos2::new(r.x, r.y), egui::vec2(r.w, r.h))
}

fn tooltip(frame: &Frame, c: &BlastCell) -> String {
    let Some(p) = c.key.and_then(|k| frame.snapshot.get(k)) else {
        return match c.kind {
            Kind::Other { count } => format!("{count} small processes"),
            _ => "Exited".to_owned(),
        };
    };
    let name = display_name(frame, p);
    let pid = p.raw.key.pid;
    let gpu = frame
        .gpu
        .as_ref()
        .map(|g| g.get(&p.raw.key).copied().unwrap_or_default())
        .map_or(String::new(), |u| {
            format!(
                "\nGPU {} · GPU memory {}",
                format::pct(Some(u.util_pct)),
                format::bytes(u.dedicated)
            )
        });
    let base = match c.kind {
        Kind::Own => format!(
            "{name} (PID {pid}) itself\nMemory {}\nCPU {}",
            format::bytes(p.raw.private_ws),
            format::pct(p.cpu_pct)
        ),
        _ => format!(
            "{name} (PID {pid})\nMemory {} in {} process(es)\nCPU {}",
            format::bytes(p.rollup.private_ws),
            p.rollup.count,
            format::pct(Some(p.rollup.cpu_pct))
        ),
    };
    base + &gpu
}

/// Draws the breadcrumb and the treemap; returns this frame's events.
pub fn show(
    ui: &mut egui::Ui,
    frame: &Frame,
    state: &mut BlastState,
    selected: Option<ProcKey>,
) -> Vec<BlastEvent> {
    let mut events = breadcrumb(ui, frame, state);
    let resp = paint(ui, frame, state, selected);
    let laid = state
        .laid
        .as_ref()
        .map_or(&[][..], |(_, _, c)| c.as_slice());
    let under = |pos: Option<Pos2>| pos.and_then(|p| hit(laid, p.x, p.y)).copied();
    if resp.double_clicked() {
        if let Some(k) = under(resp.interact_pointer_pos()).and_then(|c| c.key) {
            if zoomable(frame, k) {
                events.push(BlastEvent::Zoom(Some(k)));
            }
        }
    } else if resp.clicked() {
        if let Some(k) = under(resp.interact_pointer_pos()).and_then(|c| c.key) {
            events.push(BlastEvent::Select(k));
        }
    }
    if let Some(c) = under(resp.hover_pos()) {
        resp.on_hover_text_at_pointer(tooltip(frame, &c));
    }
    events
}

/// "Whole machine › … › zoomed process", each a way back.
fn breadcrumb(ui: &mut egui::Ui, frame: &Frame, state: &mut BlastState) -> Vec<BlastEvent> {
    let mut events = Vec::new();
    ui.horizontal_wrapped(|ui| {
        if ui
            .selectable_label(state.zoom.is_none(), "Whole machine")
            .clicked()
        {
            events.push(BlastEvent::Zoom(None));
        }
        for k in trail(frame, state.zoom) {
            ui.label("›");
            let name = frame
                .snapshot
                .get(k)
                .map_or("?", |p| display_name(frame, p));
            if ui
                .selectable_label(state.zoom == Some(k), format!("{name} ({})", k.pid))
                .clicked()
            {
                events.push(BlastEvent::Zoom(Some(k)));
            }
        }
        ui.separator();
        ui.selectable_value(&mut state.measure, Measure::Memory, "Memory · CPU")
            .on_hover_text("Size by memory, colour by CPU");
        ui.selectable_value(&mut state.measure, Measure::Gpu, "GPU memory · GPU")
            .on_hover_text("Size by dedicated GPU memory, colour by GPU use");
        ui.label(egui::RichText::new("double-click to zoom").weak().small());
    });
    events
}

/// Lays out (with hysteresis) and paints the cells; returns the map's response.
fn paint(
    ui: &mut egui::Ui,
    frame: &Frame,
    state: &mut BlastState,
    selected: Option<ProcKey>,
) -> egui::Response {
    let (resp, painter) = ui.allocate_painter(ui.available_size(), Sense::click());
    let area = resp.rect;
    let r = Rect::new(area.min.x, area.min.y, area.width(), area.height());
    state.update(frame, r);
    let laid = state.laid();
    if laid.is_empty() {
        let why = match (state.measure, &frame.gpu) {
            (Measure::Memory, _) => "No memory figures to draw yet.",
            (Measure::Gpu, None) => {
                "No GPU figures: this machine has no GPU counters (no GPU, \
                 a basic display adapter, or a remote session), or they are still starting."
            }
            (Measure::Gpu, Some(_)) => "No process is using dedicated GPU memory.",
        };
        painter.text(
            area.center(),
            egui::Align2::CENTER_CENTER,
            why,
            egui::FontId::proportional(14.0),
            ui.visuals().weak_text_color(),
        );
        return resp;
    }
    let line = Stroke::new(1.0_f32, ui.visuals().extreme_bg_color);
    let font = egui::FontId::proportional(11.0);
    for c in laid {
        let rect = to_egui(c.rect);
        if rect.area() < 1.0 {
            continue;
        }
        let gone = c.key.is_some_and(|k| frame.snapshot.get(k).is_none());
        let fill = match c.kind {
            Kind::Other { .. } => ui.visuals().faint_bg_color,
            _ if gone => Color32::from_gray(90),
            _ => heat(state.heat_of(frame, c)),
        };
        let fill = if c.kind == Kind::Group {
            fill.gamma_multiply(0.55)
        } else {
            fill
        };
        painter.rect_filled(rect, 0.0, fill);
        painter.rect_stroke(rect, 0.0, line, StrokeKind::Inside);
        let text = match (c.kind, c.key.and_then(|k| frame.snapshot.get(k))) {
            (Kind::Other { count }, _) => Some(format!("{count} small")),
            (Kind::Own, Some(p)) => Some(format!("{} itself", display_name(frame, p))),
            (_, Some(p)) => Some(display_name(frame, p).to_owned()),
            (_, None) => None,
        };
        if let Some(text) = text.filter(|_| rect.width() > 36.0 && rect.height() > 13.0) {
            painter.with_clip_rect(rect.shrink(1.0)).text(
                rect.min + egui::vec2(3.0, 2.0),
                egui::Align2::LEFT_TOP,
                text,
                font.clone(),
                Color32::WHITE,
            );
        }
    }
    if let Some(sel) = selected {
        // The innermost cell of the selection: its own block when it is a group.
        if let Some(c) = laid.iter().rev().find(|c| c.key == Some(sel)) {
            painter.rect_stroke(
                to_egui(c.rect),
                0.0,
                Stroke::new(2.5_f32, ui.visuals().selection.stroke.color),
                StrokeKind::Inside,
            );
        }
    }
    resp
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, frame_mem, k};
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    const AREA: Rect = Rect::new(0.0, 0.0, 600.0, 400.0);

    fn find(cells: &[BlastCell], pid: u32, kind: Kind) -> Option<&BlastCell> {
        cells
            .iter()
            .find(|c| c.key == Some(k(pid)) && c.kind == kind)
    }

    #[test]
    fn whole_machine_shows_roots_sized_by_subtree_memory() {
        let f = frame();
        let c = cells(&f, None, AREA);
        // explorer (650 bytes with children) and svchost (10) share the area.
        let explorer = find(&c, 1, Kind::Group).expect("explorer is a group");
        let svchost = find(&c, 9, Kind::Leaf).expect("svchost is a leaf");
        let ratio = explorer.rect.area() / svchost.rect.area();
        assert!((ratio - 65.0).abs() < 1.0, "ratio {ratio}");
    }

    #[test]
    fn a_group_holds_its_own_share_and_its_children_inside_its_header() {
        let f = frame();
        let c = cells(&f, None, AREA);
        let g = find(&c, 1, Kind::Group).unwrap().rect;
        for pid in [2, 3] {
            let child = c.iter().find(|c| c.key == Some(k(pid))).unwrap().rect;
            assert!(child.y >= g.y + HEADER - 0.01, "{pid} under the header");
            assert!(child.x >= g.x && child.x + child.w <= g.x + g.w + 0.01);
        }
        assert!(find(&c, 1, Kind::Own).is_some());
    }

    #[test]
    fn zero_memory_processes_get_no_cell_and_nothing_is_nan() {
        // svchost reads 0 bytes (Restricted memory reads as 0 in tier 0).
        let f = frame_mem([100, 300, 200, 50, 0]);
        let c = cells(&f, None, AREA);
        assert!(c.iter().all(|c| c.key != Some(k(9))));
        assert!(
            c.iter()
                .all(|c| c.rect.area().is_finite() && c.rect.area() > 0.0)
        );
        let empty = frame_mem([0; 5]);
        assert_eq!(cells(&empty, None, AREA), Vec::new());
    }

    #[test]
    fn zoom_shows_only_that_subtree_and_the_trail_leads_back() {
        let f = frame();
        let c = cells(&f, Some(k(2)), AREA);
        assert!(
            c.iter()
                .all(|c| matches!(c.key, Some(x) if x == k(2) || x == k(4)))
        );
        assert_eq!(trail(&f, Some(k(4))), vec![k(1), k(2), k(4)]);
        assert_eq!(trail(&f, None), Vec::new());
    }

    #[test]
    fn tiny_processes_merge_into_other_and_are_not_selectable() {
        // svchost at 1 byte beside 6.5 GB of explorer: far below one pixel.
        let f = frame_mem([1_000_000_000, 3_000_000_000, 2_000_000_000, 500_000_000, 1]);
        let c = cells(&f, None, AREA);
        let other = c
            .iter()
            .find(|c| matches!(c.kind, Kind::Other { .. }))
            .expect("other cell");
        assert_eq!(other.key, None);
        let mid = (
            other.rect.x + other.rect.w / 2.0,
            other.rect.y + other.rect.h / 2.0,
        );
        assert_eq!(hit(&c, mid.0, mid.1).and_then(|c| c.key), None);
    }

    #[test]
    fn hit_finds_the_innermost_cell() {
        let f = frame();
        let c = cells(&f, None, AREA);
        let child = c.iter().find(|c| c.key == Some(k(3))).unwrap().rect;
        let got = hit(&c, child.x + 1.0, child.y + 1.0).unwrap();
        assert_eq!(got.key, Some(k(3)));
        let g = find(&c, 1, Kind::Group).unwrap().rect;
        assert_eq!(hit(&c, g.x + 3.0, g.y + 3.0).unwrap().kind, Kind::Group);
    }

    #[test]
    fn layout_holds_still_under_ten_percent_and_moves_once_past_it() {
        let mut s = BlastState::default();
        let first = s.cells(&frame(), AREA).to_vec();
        // Code.exe 200 → 215 bytes: explorer's subtree moves ~2 %, Code.exe 7.5 %.
        let small = s.cells(&frame_mem([100, 300, 215, 50, 10]), AREA).to_vec();
        assert_eq!(first, small);
        // A build starts under node (4: 50 → 400): re-layout once …
        let big = frame_mem([100, 300, 200, 400, 10]);
        let moved = s.cells(&big, AREA).to_vec();
        assert_ne!(first, moved);
        // … and not again while it stays there.
        assert_eq!(s.cells(&big, AREA).to_vec(), moved);
    }

    #[test]
    fn resizing_or_zooming_lays_out_again() {
        let mut s = BlastState::default();
        let f = frame();
        let a = s.cells(&f, AREA).to_vec();
        let b = s.cells(&f, Rect::new(0.0, 0.0, 300.0, 400.0)).to_vec();
        assert_ne!(a, b);
        s.zoom = Some(k(2));
        assert!(s.cells(&f, AREA).iter().all(|c| c.key != Some(k(9))));
    }

    #[test]
    fn a_zoom_target_that_exited_falls_back_to_the_whole_machine() {
        let mut s = BlastState {
            zoom: Some(k(77)),
            ..BlastState::default()
        };
        let c = s.cells(&frame(), AREA).to_vec();
        assert_eq!(s.zoom, None);
        assert!(find(&c, 9, Kind::Leaf).is_some());
    }

    fn with_gpu(mut f: Frame, usage: &[(u32, f32, u64)]) -> Frame {
        use atropos_core::gpu::GpuUsage;
        f.gpu = Some(std::sync::Arc::new(
            usage
                .iter()
                .map(|&(pid, util_pct, dedicated)| {
                    (
                        k(pid),
                        GpuUsage {
                            util_pct,
                            dedicated,
                        },
                    )
                })
                .collect(),
        ));
        f
    }

    #[test]
    fn gpu_measure_sizes_by_dedicated_memory_and_heats_by_the_busiest_engine() {
        // node (2) holds 300 MB of GPU memory, its child 4 holds 100 MB and runs at 80 %.
        let f = with_gpu(frame(), &[(2, 10.0, 300), (4, 80.0, 100), (9, 0.0, 0)]);
        let m = measures(&f, Measure::Gpu);
        let idx = |pid| f.snapshot.index[&k(pid)];
        assert!((m.sub[idx(1)] - 400.0).abs() < 1e-9, "explorer's subtree");
        assert!((m.sub[idx(2)] - 400.0).abs() < 1e-9);
        assert!((m.own[idx(2)] - 300.0).abs() < 1e-9);
        assert!(
            (m.sub_heat[idx(1)] - 0.8).abs() < 1e-6,
            "busiest in the subtree"
        );
        assert!((m.own_heat[idx(2)] - 0.1).abs() < 1e-6);
        let c = cells_by(&f, &m, None, AREA);
        assert!(c.iter().all(|c| c.key != Some(k(9)) && c.key != Some(k(3))));
    }

    #[test]
    fn gpu_measure_without_gpu_figures_draws_nothing() {
        let f = frame();
        let m = measures(&f, Measure::Gpu);
        assert_eq!(cells_by(&f, &m, None, AREA), Vec::new());
        let mut s = BlastState {
            measure: Measure::Gpu,
            ..BlastState::default()
        };
        assert_eq!(s.cells(&f, AREA), &[]);
        // Switching back lays out by memory again.
        s.measure = Measure::Memory;
        assert_ne!(s.cells(&f, AREA).len(), 0);
    }

    #[test]
    fn heat_runs_from_calm_to_hot_and_survives_nan() {
        assert_eq!(heat(0.0), Color32::from_rgb(52, 96, 128));
        assert_eq!(heat(1.0), Color32::from_rgb(214, 64, 44));
        assert_eq!(heat(5.0), heat(1.0));
        assert_eq!(heat(f32::NAN), heat(0.0));
    }

    // ---- kittest: the widget ----

    struct S {
        frame: Frame,
        state: BlastState,
        selected: Option<ProcKey>,
        events: Vec<BlastEvent>,
        acts: Vec<(crate::keys::Action, ProcKey)>,
    }

    fn harness() -> Harness<'static, S> {
        Harness::builder()
            .with_size(egui::vec2(600.0, 440.0))
            // egui counts a double-click within 0.3 s; the default step is 0.25 s per frame.
            .with_step_dt(1.0 / 60.0)
            .build_ui_state(
                |ui, s: &mut S| {
                    for e in show(ui, &s.frame, &mut s.state, s.selected) {
                        match e {
                            BlastEvent::Select(k) => s.selected = Some(k),
                            BlastEvent::Zoom(z) => s.state.zoom = z,
                        }
                        s.events.push(e);
                    }
                    // The app's key handling: kill keys act on the selection, whatever the view.
                    let typing = ui.ctx().wants_keyboard_input();
                    for a in ui.input(|i| crate::keys::actions(&i.events, typing)) {
                        if let Some(k) = s.selected {
                            s.acts.push((a, k));
                        }
                    }
                },
                S {
                    frame: frame(),
                    state: BlastState::default(),
                    selected: None,
                    events: Vec::new(),
                    acts: Vec::new(),
                },
            )
    }

    fn centre_of(h: &Harness<'_, S>, pid: u32, kind: Kind) -> Pos2 {
        let c = h
            .state()
            .state
            .laid
            .as_ref()
            .and_then(|(_, _, c)| find(c, pid, kind).copied())
            .expect("cell laid out");
        to_egui(c.rect).center()
    }

    fn click(h: &mut Harness<'_, S>, at: Pos2) {
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
    }

    #[test]
    fn click_selects_the_process_under_the_pointer() {
        let mut h = harness();
        h.run();
        let at = centre_of(&h, 3, Kind::Leaf);
        click(&mut h, at);
        assert_eq!(h.state().selected, Some(k(3)));
    }

    #[test]
    fn double_click_zooms_and_the_breadcrumb_goes_back() {
        let mut h = harness();
        h.run();
        // On the group's header strip: the centre may be one of its children.
        let at = centre_of(&h, 2, Kind::Own);
        click(&mut h, at);
        click(&mut h, at);
        assert_eq!(h.state().state.zoom, Some(k(2)));
        assert!(h.query_by_label("node.exe (2)").is_some());
        h.get_by_label("Whole machine").click();
        h.run();
        assert_eq!(h.state().state.zoom, None);
    }

    #[test]
    fn delete_after_a_click_asks_to_kill_that_process() {
        let mut h = harness();
        h.run();
        let at = centre_of(&h, 3, Kind::Leaf);
        click(&mut h, at);
        h.key_press(egui::Key::Delete);
        h.run();
        h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::Delete);
        h.run();
        assert_eq!(
            h.state().acts,
            vec![
                (crate::keys::Action::Kill, k(3)),
                (crate::keys::Action::KillTree, k(3))
            ]
        );
    }
}
