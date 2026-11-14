//! The Settings window. Every change is saved at once (the file appears only then, Z3).

use atropos_core::config::{Config, Confirm, Theme, View};
use eframe::egui;

/// Window state.
#[derive(Debug, Default)]
pub struct SettingsUi {
    pub open: bool,
    /// The comma-separated extra secret names being edited.
    mask_text: Option<String>,
}

/// Draws the window; returns whether `cfg` changed.
pub fn show(ctx: &egui::Context, ui_state: &mut SettingsUi, cfg: &mut Config) -> bool {
    let before = cfg.clone();
    let mut open = ui_state.open;
    egui::Window::new("Settings")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            egui::Grid::new("settings")
                .num_columns(2)
                .spacing([16.0, 10.0])
                .show(ui, |ui| {
                    ui.label("Refresh");
                    ui.horizontal(|ui| {
                        let mut fixed = cfg.tick_secs.is_some();
                        ui.radio_value(&mut fixed, false, "Adaptive")
                            .on_hover_text("1 s focused, 2 s in the background, 5 s minimized");
                        ui.radio_value(&mut fixed, true, "Every");
                        if fixed {
                            let mut v = cfg.tick_secs.unwrap_or(1.0);
                            ui.add(egui::Slider::new(&mut v, 0.5..=10.0).suffix(" s"));
                            cfg.tick_secs = Some(v);
                        } else {
                            cfg.tick_secs = None;
                        }
                    });
                    ui.end_row();

                    ui.label("Grace period");
                    ui.add(egui::Slider::new(&mut cfg.grace_secs, 1..=30).suffix(" s"))
                        .on_hover_text("How long a process may take to close before it is forced");
                    ui.end_row();

                    ui.label("Confirm before ending");
                    ui.vertical(|ui| {
                        ui.radio_value(&mut cfg.confirm, Confirm::Always, "Always");
                        ui.radio_value(&mut cfg.confirm, Confirm::Risky, "Only for risky targets")
                            .on_hover_text(
                                "The desktop, other users' and multi-service processes still ask",
                            );
                    });
                    ui.end_row();

                    ui.label("Theme");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut cfg.theme, Theme::System, "System");
                        ui.radio_value(&mut cfg.theme, Theme::Light, "Light");
                        ui.radio_value(&mut cfg.theme, Theme::Dark, "Dark");
                    });
                    ui.end_row();

                    ui.label("Start in");
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut cfg.default_view, View::Flat, "Flat list");
                        ui.radio_value(&mut cfg.default_view, View::Tree, "Tree");
                    });
                    ui.end_row();

                    ui.label("Extra secret names");
                    let text = ui_state
                        .mask_text
                        .get_or_insert_with(|| cfg.mask_names.join(", "));
                    let r = ui
                        .add(egui::TextEdit::singleline(text).hint_text("e.g. INTERNAL, CORP_"))
                        .on_hover_text(
                            "Environment variables whose name contains one of these are hidden",
                        );
                    if r.lost_focus() {
                        cfg.mask_names = text
                            .split(',')
                            .map(|t| t.trim().to_owned())
                            .filter(|t| !t.is_empty())
                            .collect();
                    }
                    ui.end_row();
                });
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(atropos_core::config::path().display().to_string())
                    .weak()
                    .small(),
            );
        });
    ui_state.open = open;
    if !open {
        ui_state.mask_text = None;
    }
    *cfg != before
}

/// The egui theme for a setting.
#[must_use]
pub fn theme(t: Theme) -> egui::ThemePreference {
    match t {
        Theme::System => egui::ThemePreference::System,
        Theme::Light => egui::ThemePreference::Light,
        Theme::Dark => egui::ThemePreference::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    struct S {
        ui: SettingsUi,
        cfg: Config,
        changes: usize,
    }

    fn harness() -> Harness<'static, S> {
        Harness::builder()
            .with_size(egui::vec2(700.0, 700.0))
            .build_state(
                |ctx, s: &mut S| {
                    if show(ctx, &mut s.ui, &mut s.cfg) {
                        s.changes += 1;
                    }
                },
                S {
                    ui: SettingsUi {
                        open: true,
                        mask_text: None,
                    },
                    cfg: Config::default(),
                    changes: 0,
                },
            )
    }

    #[test]
    fn choosing_options_changes_the_config() {
        let mut h = harness();
        h.run();
        h.get_by_label("Dark").click();
        h.run();
        assert_eq!(h.state().cfg.theme, Theme::Dark);
        h.get_by_label("Only for risky targets").click();
        h.run();
        assert_eq!(h.state().cfg.confirm, Confirm::Risky);
        h.get_by_label("Tree").click();
        h.run();
        assert_eq!(h.state().cfg.default_view, View::Tree);
        assert!(h.state().changes >= 3);
    }

    #[test]
    fn nothing_changes_while_just_looking() {
        let mut h = harness();
        h.run();
        h.run();
        assert_eq!(h.state().changes, 0);
        assert_eq!(h.state().cfg, Config::default());
    }
}
