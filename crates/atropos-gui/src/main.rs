//! `Atropos.exe`: GUI subsystem, so no console window ever flashes.
#![windows_subsystem = "windows"]

#[cfg(all(feature = "renderer-glow", feature = "renderer-wgpu"))]
compile_error!("enable exactly one of `renderer-glow` / `renderer-wgpu`");
#[cfg(not(any(feature = "renderer-glow", feature = "renderer-wgpu")))]
compile_error!("enable one of `renderer-glow` / `renderer-wgpu`");

mod actions;
mod app;
mod args;
mod crash;
mod details;
mod dialog;
mod elevate;
mod fatal;
mod format;
mod header;
mod icon;
mod keys;
mod logging;
mod selection;
mod settings;
mod startup;
mod table;
#[cfg(test)]
mod test_frame;
mod toast;
mod view;

use eframe::egui;

/// Counts Atropos's own heap; drivers allocate elsewhere (ADR 0005).
#[global_allocator]
pub static ALLOC: atropos_core::platform::windows::CountingAlloc =
    atropos_core::platform::windows::CountingAlloc::new();

#[cfg(feature = "renderer-glow")]
const RENDERER: (eframe::Renderer, &str) = (eframe::Renderer::Glow, "OpenGL (glow)");
#[cfg(feature = "renderer-wgpu")]
const RENDERER: (eframe::Renderer, &str) = (eframe::Renderer::Wgpu, "wgpu");

fn main() -> std::process::ExitCode {
    // Elevation helper mode (spec §4.7): no window, serve the pipe, exit when idle.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    if raw.first().map(String::as_str) == Some("--elevated-helper") {
        let _log_guard = logging::init().ok();
        return match atropos_helper::host::parse_args(&raw) {
            Some(a) => atropos_helper::host::run(&a),
            None => std::process::ExitCode::from(2),
        };
    }
    // Read our own identity first so startup timing is measured from process creation.
    let create_time =
        atropos_core::platform::windows::current_process_key().map_or(0, |k| k.create_time);
    let _log_guard = logging::init().ok();
    fatal::install_panic_hook(RENDERER.1);
    let args = args::Args::parse(std::env::args_os().skip(1));

    let options = eframe::NativeOptions {
        renderer: RENDERER.0,
        viewport: egui::ViewportBuilder::default()
            .with_title("Atropos — Cut the right thread")
            .with_inner_size([1100.0, 680.0])
            .with_min_inner_size([480.0, 280.0])
            .with_icon(icon::window_icon()),
        ..Default::default()
    };
    let result = eframe::run_native(
        "Atropos",
        options,
        Box::new(move |cc| Ok(Box::new(app::AtroposApp::new(cc, args, create_time)))),
    );
    match result {
        Ok(()) => std::process::ExitCode::SUCCESS,
        Err(e) => {
            tracing::error!("renderer {} failed to start: {e}", RENDERER.1);
            atropos_core::platform::windows::fatal_message(
                "Atropos could not start",
                &fatal::body(RENDERER.1, &e.to_string(), &atropos_core::paths::log_dir()),
            );
            std::process::ExitCode::FAILURE
        }
    }
}
