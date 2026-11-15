//! Squarified treemap layout (Bruls, Huizing, van Wijk 2000) and layout hysteresis (spec
//! Phase 5). Pure geometry: no egui, no processes. The GUI nests it to draw the process tree.

use std::collections::HashMap;
use std::hash::Hash;

/// Cells smaller than this (px²) are merged into one [`Slot::Other`] cell instead of drawn.
pub const MIN_AREA: f64 = 1.0;
/// Re-layout only when a size moved by more than this fraction since the last layout.
pub const HYSTERESIS: f64 = 0.10;

/// An axis-aligned rectangle in pixels.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
}

impl Rect {
    #[must_use]
    pub const fn new(x: f32, y: f32, w: f32, h: f32) -> Self {
        Self { x, y, w, h }
    }

    #[must_use]
    pub fn area(&self) -> f32 {
        self.w * self.h
    }
}

/// One thing to lay out; `size` is in any unit (bytes). Non-finite or non-positive sizes are
/// skipped.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Item<K> {
    pub key: K,
    pub size: f64,
}

/// What a cell stands for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Slot<K> {
    Item(K),
    /// `count` items too small to draw, merged. Never selectable.
    Other {
        count: usize,
    },
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Cell<K> {
    pub slot: Slot<K>,
    pub rect: Rect,
}

fn usable(size: f64) -> bool {
    size.is_finite() && size > 0.0
}

/// Lays `items` out in `rect`, largest first. Areas are proportional to sizes, the cells tile
/// `rect` without overlap, and items whose cell would be below [`MIN_AREA`] share one
/// [`Slot::Other`] cell.
#[must_use]
pub fn layout<K: Copy>(items: &[Item<K>], rect: Rect) -> Vec<Cell<K>> {
    let area = f64::from(rect.w) * f64::from(rect.h);
    if !(area.is_finite() && area > 0.0) {
        return Vec::new();
    }
    let total: f64 = items.iter().map(|i| i.size).filter(|s| usable(*s)).sum();
    if !(total.is_finite() && total > 0.0) {
        return Vec::new();
    }
    let scale = area / total;
    let mut slots: Vec<(Slot<K>, f64)> = Vec::with_capacity(items.len());
    let (mut other_count, mut other_area) = (0usize, 0.0f64);
    for i in items.iter().filter(|i| usable(i.size)) {
        let a = i.size * scale;
        if a < MIN_AREA {
            other_count += 1;
            other_area += a;
        } else {
            slots.push((Slot::Item(i.key), a));
        }
    }
    if other_count > 0 {
        slots.push((Slot::Other { count: other_count }, other_area));
    }
    // Stable: equal sizes keep their input order, so the layout is deterministic.
    slots.sort_by(|a, b| b.1.total_cmp(&a.1));
    let areas: Vec<f64> = slots.iter().map(|s| s.1).collect();
    squarify(&areas, rect)
        .into_iter()
        .zip(slots)
        .map(|(rect, (slot, _))| Cell { slot, rect })
        .collect()
}

/// The worst aspect ratio of a row of `areas` laid along a side of length `side`.
fn worst(sum: f64, min: f64, max: f64, side: f64) -> f64 {
    let s2 = side * side;
    let sum2 = sum * sum;
    (s2 * max / sum2).max(sum2 / (s2 * min))
}

#[allow(clippy::cast_possible_truncation, clippy::many_single_char_names)] // pixel geometry
fn squarify(areas: &[f64], rect: Rect) -> Vec<Rect> {
    let (mut x, mut y) = (f64::from(rect.x), f64::from(rect.y));
    let (mut w, mut h) = (f64::from(rect.w), f64::from(rect.h));
    let mut out = Vec::with_capacity(areas.len());
    let mut i = 0;
    while i < areas.len() {
        let side = w.min(h);
        // Grow the row while its worst aspect ratio improves.
        let mut end = i + 1;
        let (mut sum, mut min, mut max) = (areas[i], areas[i], areas[i]);
        let mut best = worst(sum, min, max, side);
        while end < areas.len() {
            let a = areas[end];
            let next = worst(sum + a, min.min(a), max.max(a), side);
            if next > best {
                break;
            }
            best = next;
            sum += a;
            min = min.min(a);
            max = max.max(a);
            end += 1;
        }
        let last_row = end == areas.len();
        if w >= h {
            // A column on the left, as tall as the rectangle.
            let cw = if last_row { w } else { sum / h };
            let mut cy = y;
            for (n, a) in areas[i..end].iter().enumerate() {
                let ch = if n + 1 == end - i { y + h - cy } else { a / cw };
                out.push(Rect::new(x as f32, cy as f32, cw as f32, ch as f32));
                cy += ch;
            }
            x += cw;
            w -= cw;
        } else {
            // A row on top, as wide as the rectangle.
            let rh = if last_row { h } else { sum / w };
            let mut cx = x;
            for (n, a) in areas[i..end].iter().enumerate() {
                let cw = if n + 1 == end - i { x + w - cx } else { a / rh };
                out.push(Rect::new(cx as f32, y as f32, cw as f32, rh as f32));
                cx += cw;
            }
            y += rh;
            h -= rh;
        }
        i = end;
    }
    out
}

/// Decides when a treemap must be laid out again, so rectangles hold still while sizes jitter.
#[derive(Debug, Clone)]
pub struct Hysteresis<K> {
    /// Sizes at the last layout; `None` before the first.
    at_layout: Option<HashMap<K, f64>>,
}

impl<K> Default for Hysteresis<K> {
    fn default() -> Self {
        Self { at_layout: None }
    }
}

impl<K: Copy + Eq + Hash> Hysteresis<K> {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Forces the next [`update`](Self::update) to re-layout (zoom, resize, mode change).
    pub fn reset(&mut self) {
        self.at_layout = None;
    }

    /// Whether to re-layout for these sizes: the first call, or any size that moved by more than
    /// [`HYSTERESIS`] since the last layout (appearing and disappearing count as moves from and
    /// to zero). Sizes below `ignore_below` both before and after are ignored: those items are
    /// in the "other" cell anyway. Records the sizes when it returns `true`.
    pub fn update(&mut self, sizes: &[(K, f64)], ignore_below: f64) -> bool {
        let clean = |s: f64| if usable(s) { s } else { 0.0 };
        let now: HashMap<K, f64> = sizes.iter().map(|&(k, s)| (k, clean(s))).collect();
        let moved = |old: f64, new: f64| {
            !(old < ignore_below && new < ignore_below) && (new - old).abs() > HYSTERESIS * old
        };
        let changed = match &self.at_layout {
            None => true,
            Some(prev) => {
                now.iter()
                    .any(|(k, &new)| moved(prev.get(k).copied().unwrap_or(0.0), new))
                    || prev
                        .iter()
                        .any(|(k, &old)| !now.contains_key(k) && moved(old, 0.0))
            }
        };
        if changed {
            self.at_layout = Some(now);
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn items(sizes: &[f64]) -> Vec<Item<usize>> {
        sizes
            .iter()
            .enumerate()
            .map(|(key, &size)| Item { key, size })
            .collect()
    }

    fn area_of(cells: &[Cell<usize>], key: usize) -> f32 {
        cells
            .iter()
            .find(|c| c.slot == Slot::Item(key))
            .map(|c| c.rect.area())
            .unwrap_or_default()
    }

    fn overlap(a: Rect, b: Rect) -> f32 {
        let w = (a.x + a.w).min(b.x + b.w) - a.x.max(b.x);
        let h = (a.y + a.h).min(b.y + b.h) - a.y.max(b.y);
        // f32 coordinates near 1000 px carry ~1e-4 px of rounding.
        if w > 0.01 && h > 0.01 { w * h } else { 0.0 }
    }

    fn aspect(r: Rect) -> f32 {
        (r.w / r.h).max(r.h / r.w)
    }

    #[test]
    fn areas_are_proportional_to_sizes() {
        let cells = layout(&items(&[6.0, 3.0, 1.0]), Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(cells.len(), 3);
        for (key, want) in [(0, 6000.0), (1, 3000.0), (2, 1000.0)] {
            assert!(
                (area_of(&cells, key) - want).abs() < 1.0,
                "{key}: {cells:?}"
            );
        }
    }

    #[test]
    fn largest_item_comes_first() {
        let cells = layout(&items(&[1.0, 9.0, 3.0]), Rect::new(0.0, 0.0, 30.0, 10.0));
        assert_eq!(cells[0].slot, Slot::Item(1));
    }

    #[test]
    fn zero_negative_and_non_finite_sizes_are_skipped() {
        let cells = layout(
            &items(&[0.0, f64::NAN, -5.0, f64::INFINITY, 4.0]),
            Rect::new(0.0, 0.0, 10.0, 10.0),
        );
        assert_eq!(cells.len(), 1);
        assert_eq!(cells[0].slot, Slot::Item(4));
        assert!((cells[0].rect.area() - 100.0).abs() < 0.01);
    }

    #[test]
    fn nothing_to_lay_out_gives_no_cells() {
        let r = Rect::new(0.0, 0.0, 10.0, 10.0);
        assert_eq!(layout(&items(&[0.0, f64::NAN]), r), Vec::new());
        assert_eq!(layout::<usize>(&[], r), Vec::new());
        assert_eq!(
            layout(&items(&[1.0]), Rect::new(0.0, 0.0, 0.0, 10.0)),
            Vec::new()
        );
        assert_eq!(
            layout(&items(&[1.0]), Rect::new(0.0, 0.0, f32::NAN, 10.0)),
            Vec::new()
        );
    }

    #[test]
    fn sub_pixel_items_are_merged_into_one_other_cell() {
        let mut sizes = vec![1_000_000.0];
        sizes.extend(std::iter::repeat_n(1.0, 1000));
        let cells = layout(&items(&sizes), Rect::new(0.0, 0.0, 100.0, 100.0));
        assert_eq!(cells.len(), 2, "{cells:?}");
        assert_eq!(cells[0].slot, Slot::Item(0));
        assert_eq!(cells[1].slot, Slot::Other { count: 1000 });
        assert!(
            cells
                .iter()
                .filter(|c| matches!(c.slot, Slot::Item(_)))
                .all(|c| f64::from(c.rect.area()) >= MIN_AREA)
        );
    }

    #[test]
    fn aspect_ratios_stay_bounded_on_typical_input() {
        // Process memory: a few large, many medium, a long tail.
        let sizes: Vec<f64> = (1..=60).map(|i| 1e9 / f64::from(i * i)).collect();
        let cells = layout(&items(&sizes), Rect::new(0.0, 0.0, 1200.0, 700.0));
        let ratios: Vec<f32> = cells.iter().map(|c| aspect(c.rect)).collect();
        #[allow(clippy::cast_precision_loss)]
        let mean = ratios.iter().sum::<f32>() / ratios.len() as f32;
        assert!(mean < 3.0, "mean aspect {mean}");
        assert!(ratios.iter().all(|r| *r < 12.0), "{ratios:?}");
    }

    proptest! {
        #[test]
        fn cells_tile_the_rectangle(
            sizes in prop::collection::vec(0.5f64..1000.0, 1..80),
            w in 20f32..1500.0,
            h in 20f32..1000.0,
        ) {
            let rect = Rect::new(5.0, 7.0, w, h);
            let cells = layout(&items(&sizes), rect);
            let sum: f32 = cells.iter().map(|c| c.rect.area()).sum();
            prop_assert!((sum - rect.area()).abs() <= rect.area() * 1e-3, "sum {} vs {}", sum, rect.area());
            for (i, a) in cells.iter().enumerate() {
                let r = a.rect;
                prop_assert!(r.w.is_finite() && r.h.is_finite() && r.w >= 0.0 && r.h >= 0.0);
                prop_assert!(r.x >= rect.x - 0.01 && r.y >= rect.y - 0.01);
                prop_assert!(r.x + r.w <= rect.x + rect.w + 0.01);
                prop_assert!(r.y + r.h <= rect.y + rect.h + 0.01);
                for b in &cells[i + 1..] {
                    prop_assert!(overlap(r, b.rect) < 0.01, "{:?} overlaps {:?}", r, b.rect);
                }
            }
            // Every item is either drawn or counted in "other", never both, never lost.
            let drawn = cells.iter().filter(|c| matches!(c.slot, Slot::Item(_))).count();
            let other: usize = cells.iter().map(|c| match c.slot { Slot::Other { count } => count, Slot::Item(_) => 0 }).sum();
            prop_assert_eq!(drawn + other, sizes.len());
        }
    }

    fn sizes(v: &[(u32, f64)]) -> Vec<(u32, f64)> {
        v.to_vec()
    }

    #[test]
    fn hysteresis_lays_out_first_then_ignores_small_moves() {
        let mut h = Hysteresis::new();
        assert!(h.update(&sizes(&[(1, 100.0), (2, 50.0)]), 1.0));
        assert!(!h.update(&sizes(&[(1, 105.0), (2, 46.0)]), 1.0));
        assert!(!h.update(&sizes(&[(1, 109.9), (2, 50.0)]), 1.0));
    }

    #[test]
    fn hysteresis_lays_out_again_past_ten_percent() {
        let mut h = Hysteresis::new();
        assert!(h.update(&sizes(&[(1, 100.0)]), 1.0));
        assert!(h.update(&sizes(&[(1, 111.0)]), 1.0));
        // Measured against the new layout now: 111 → 115 is under 10 %.
        assert!(!h.update(&sizes(&[(1, 115.0)]), 1.0));
    }

    #[test]
    fn hysteresis_catches_slow_drift() {
        let mut h = Hysteresis::new();
        assert!(h.update(&sizes(&[(1, 100.0)]), 1.0));
        assert!(!h.update(&sizes(&[(1, 105.0)]), 1.0));
        assert!(!h.update(&sizes(&[(1, 109.0)]), 1.0));
        assert!(h.update(&sizes(&[(1, 112.0)]), 1.0));
    }

    #[test]
    fn hysteresis_on_appearing_and_disappearing_items() {
        let mut h = Hysteresis::new();
        assert!(h.update(&sizes(&[(1, 100.0), (2, 0.5)]), 1.0));
        // A tiny newcomer and a tiny leaver do not move anything visible.
        assert!(!h.update(&sizes(&[(1, 100.0), (3, 0.4)]), 1.0));
        // A visible newcomer does.
        assert!(h.update(&sizes(&[(1, 100.0), (3, 0.4), (4, 30.0)]), 1.0));
        // So does a visible leaver.
        assert!(h.update(&sizes(&[(1, 100.0), (3, 0.4)]), 1.0));
    }

    #[test]
    fn hysteresis_treats_nan_as_zero_and_reset_forces_layout() {
        let mut h = Hysteresis::new();
        assert!(h.update(&sizes(&[(1, 100.0), (2, f64::NAN)]), 1.0));
        assert!(!h.update(&sizes(&[(1, 100.0), (2, f64::NAN)]), 1.0));
        h.reset();
        assert!(h.update(&sizes(&[(1, 100.0)]), 1.0));
    }
}
