//! "Export snapshot…" (spec Phase 6): writes the current frame as JSON for a bug report.
//! Secret-looking command-line values are masked unless the user ticks a box that says what
//! that means (the confirmation for an unmasked export).

use std::ffi::OsString;
use std::path::PathBuf;

use eframe::egui;

/// Window state.
#[derive(Debug, Default)]
pub struct ExportUi {
    pub open: bool,
    unmasked: bool,
    /// Where the last export went, or why it failed.
    pub result: Option<Result<PathBuf, String>>,
}

/// Where exports go: the Downloads folder when there is one, else next to the logs.
#[must_use]
pub fn folder(
    get: impl Fn(&str) -> Option<OsString>,
    is_dir: impl Fn(&std::path::Path) -> bool,
) -> PathBuf {
    if let Some(home) = get("USERPROFILE").filter(|v| !v.is_empty()) {
        let downloads = PathBuf::from(home).join("Downloads");
        if is_dir(&downloads) {
            return downloads;
        }
    }
    atropos_core::paths::log_dir_from(get)
        .parent()
        .map_or_else(std::env::temp_dir, std::path::Path::to_path_buf)
        .join("exports")
}

/// `atropos-snapshot-YYYYMMDD-HHMMSS.json`.
#[must_use]
pub fn file_name(t: Option<atropos_core::platform::windows::LocalTime>) -> String {
    match t {
        Some(t) => format!(
            "atropos-snapshot-{:04}{:02}{:02}-{:02}{:02}{:02}.json",
            t.year, t.month, t.day, t.hour, t.minute, t.second
        ),
        None => "atropos-snapshot.json".to_owned(),
    }
}

/// Draws the window; returns `Some(unmasked)` when the user clicks Export.
pub fn show(ctx: &egui::Context, st: &mut ExportUi) -> Option<bool> {
    let mut asked = None;
    let mut open = st.open;
    egui::Window::new("Export snapshot")
        .open(&mut open)
        .collapsible(false)
        .resizable(false)
        .show(ctx, |ui| {
            ui.label(
                "Every process with its tree, command line, user, label, folder and ports, as \
                 JSON for a bug report. Environment variables are never included.",
            );
            ui.checkbox(
                &mut st.unmasked,
                "Include passwords and tokens found in command lines",
            );
            if st.unmasked {
                ui.colored_label(
                    ui.visuals().warn_fg_color,
                    "The file will then show those secrets to anyone you send it to.",
                );
            }
            if ui.button("Export").clicked() {
                asked = Some(st.unmasked);
            }
            match &st.result {
                Some(Ok(p)) => {
                    ui.label(format!("Saved to {}", p.display()));
                }
                Some(Err(e)) => {
                    ui.colored_label(ui.visuals().error_fg_color, e);
                }
                None => {}
            }
        });
    st.open = open;
    asked
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable as _;

    #[test]
    fn exports_go_to_downloads_when_it_exists() {
        let home = r"C:\Users\José Müller";
        let get = |k: &str| (k == "USERPROFILE").then(|| OsString::from(home));
        assert_eq!(folder(get, |_| true), PathBuf::from(home).join("Downloads"));
        let local = |k: &str| match k {
            "LOCALAPPDATA" => Some(OsString::from(r"C:\L")),
            _ => None,
        };
        assert_eq!(
            folder(local, |_| false),
            PathBuf::from(r"C:\L").join("Atropos").join("exports")
        );
    }

    #[test]
    fn the_file_name_carries_the_time() {
        let t = atropos_core::platform::windows::LocalTime {
            year: 2026,
            month: 11,
            day: 15,
            hour: 9,
            minute: 5,
            second: 7,
        };
        assert_eq!(file_name(Some(t)), "atropos-snapshot-20261115-090507.json");
        assert_eq!(file_name(None), "atropos-snapshot.json");
    }

    struct S {
        ui: ExportUi,
        asked: Vec<bool>,
    }

    fn harness() -> Harness<'static, S> {
        Harness::builder()
            .with_size(egui::vec2(600.0, 400.0))
            .build_state(
                |ctx, s: &mut S| {
                    if let Some(u) = show(ctx, &mut s.ui) {
                        s.asked.push(u);
                    }
                },
                S {
                    ui: ExportUi {
                        open: true,
                        ..ExportUi::default()
                    },
                    asked: Vec::new(),
                },
            )
    }

    #[test]
    fn exports_are_masked_unless_the_box_is_ticked() {
        let mut h = harness();
        h.run();
        h.get_by_label("Export").click();
        h.run();
        h.get_by_label_contains("Include passwords and tokens")
            .click();
        h.run();
        assert!(h.query_by_label_contains("anyone you send").is_some());
        h.get_by_label("Export").click();
        h.run();
        assert_eq!(h.state().asked, vec![false, true]);
    }
}
