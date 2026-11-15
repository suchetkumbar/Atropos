//! The Settings window. Every change is saved at once (the file appears only then, Z3).

use atropos_core::config::{Config, Confirm, Theme, View};
use eframe::egui;

/// Window state.
#[derive(Debug, Default)]
pub struct SettingsUi {
    pub open: bool,
    /// The comma-separated extra secret names being edited.
    mask_text: Option<String>,
    /// The hotkey being edited, and why the last value was refused.
    hotkey_text: Option<String>,
    hotkey_err: Option<String>,
    /// Whether the Explorer menu entry is installed, as the app read it when the window opened
    /// (`None`: not known, the box is hidden). Settings itself never touches the registry.
    pub explorer_menu: Option<bool>,
    /// Set when the user ticks / unticks the box; the app installs or removes the entry.
    pub explorer_request: Option<bool>,
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

                    hotkey_row(ui, ui_state, cfg);

                    if let Some(mut installed) = ui_state.explorer_menu {
                        ui.label("Explorer");
                        let r = ui
                            .checkbox(&mut installed, "Add \"What is locking this?\" to Explorer")
                            .on_hover_text(
                                "A right-click entry for files and folders, for your account \
                                 only (Windows 11: under \"Show more options\")",
                            );
                        if r.changed() {
                            ui_state.explorer_request = Some(installed);
                        }
                        ui.end_row();
                    }
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
        ui_state.hotkey_text = None;
        ui_state.hotkey_err = None;
    }
    *cfg != before
}

/// "Global hotkey": checked when the field loses focus; a refused value keeps the old one.
fn hotkey_row(ui: &mut egui::Ui, ui_state: &mut SettingsUi, cfg: &mut Config) {
    let label = ui.label("Global hotkey");
    ui.vertical(|ui| {
        let text = ui_state
            .hotkey_text
            .get_or_insert_with(|| cfg.hotkey.clone());
        let r = ui
            .add(egui::TextEdit::singleline(text).hint_text(atropos_core::config::DEFAULT_HOTKEY))
            .labelled_by(label.id)
            .on_hover_text("Opens the command palette from any program, e.g. Ctrl+Alt+K");
        if r.lost_focus() {
            match crate::summon::parse_hotkey(text) {
                Ok(_) => {
                    text.trim().clone_into(&mut cfg.hotkey);
                    ui_state.hotkey_err = None;
                }
                Err(e) => ui_state.hotkey_err = Some(e),
            }
        }
        if let Some(e) = &ui_state.hotkey_err {
            ui.colored_label(ui.visuals().error_fg_color, e);
        }
    });
    ui.end_row();
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
                        ..SettingsUi::default()
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

    /// Types `text` into the hotkey field and moves focus away (the value is checked then).
    fn set_hotkey(h: &mut Harness<'static, S>, text: &str) {
        let field = h.get_by_label("Global hotkey");
        field.focus();
        h.run();
        h.input_mut().events.push(egui::Event::Key {
            key: egui::Key::A,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers: egui::Modifiers::COMMAND,
        });
        h.input_mut()
            .events
            .push(egui::Event::Text(text.to_owned()));
        h.run();
        h.key_press(egui::Key::Enter);
        h.run();
    }

    #[test]
    fn a_valid_hotkey_is_taken() {
        let mut h = harness();
        h.run();
        set_hotkey(&mut h, "Ctrl+Shift+Space");
        assert_eq!(h.state().cfg.hotkey, "Ctrl+Shift+Space");
    }

    #[test]
    fn a_hotkey_without_ctrl_alt_or_win_is_refused_with_a_reason() {
        let mut h = harness();
        h.run();
        set_hotkey(&mut h, "Shift+K");
        assert_eq!(h.state().cfg.hotkey, atropos_core::config::DEFAULT_HOTKEY);
        assert!(
            h.query_by_label_contains("needs Ctrl, Alt or Win")
                .is_some()
        );
    }

    #[test]
    fn the_explorer_box_is_hidden_until_the_app_says_what_is_installed() {
        let mut h = harness();
        h.run();
        assert!(h.query_by_label_contains("Explorer").is_none());
        h.state_mut().ui.explorer_menu = Some(false);
        h.run();
        h.get_by_label("Add \"What is locking this?\" to Explorer")
            .click();
        h.run();
        // Only a request: the app does the registry work.
        assert_eq!(h.state().ui.explorer_request, Some(true));
        assert_eq!(h.state().ui.explorer_menu, Some(false));
    }
}
