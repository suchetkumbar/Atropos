//! The text of the fatal-error dialog: the GUI has no console, so this is the user's only clue.

use std::path::Path;

/// Builds the dialog body for a startup failure or a crash.
pub fn body(renderer: &str, detail: &str, log_dir: &Path) -> String {
    format!(
        "The {renderer} renderer failed:\n\n{detail}\n\nThis can happen on virtual machines or remote sessions without graphics acceleration.{}\nLog folder: {}",
        if cfg!(windows) {
            ""
        } else {
            "\nIn a terminal without a desktop (e.g. over SSH), run atropos-tui instead."
        },
        log_dir.display()
    )
}

/// Makes a panic visible: log it, then show the fatal dialog (otherwise the window just vanishes).
pub fn install_panic_hook(renderer: &'static str) {
    std::panic::set_hook(Box::new(move |info| {
        tracing::error!("panic: {info}");
        atropos_core::platform::fatal_message(
            "Atropos stopped unexpectedly",
            &body(renderer, &info.to_string(), &atropos_core::paths::log_dir()),
        );
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn body_names_renderer_detail_and_log_folder() {
        let b = body(
            "wgpu",
            "No backend enabled",
            Path::new(r"C:\Users\Dév\AppData\Local\Atropos\logs"),
        );
        assert!(b.contains("wgpu"));
        assert!(b.contains("No backend enabled"));
        assert!(b.contains(r"C:\Users\Dév\AppData\Local\Atropos\logs"));
        assert!(b.contains("virtual machines or remote sessions"));
    }
}
