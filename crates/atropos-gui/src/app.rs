//! The (empty) main window. Repaints only on input: no `request_repaint` calls in Phase 0.

use std::io::Write as _;

use eframe::egui;
use egui_extras::{Column, TableBuilder};

use crate::args::Args;
use crate::startup;

const COLUMNS: [&str; 10] = [
    "Name", "PID", "CPU %", "Memory", "Threads", "Handles", "Disk R/s", "Disk W/s", "User",
    "Started",
];
const ROW_HEIGHT: f32 = 20.0;

pub struct AtroposApp {
    args: Args,
    create_time: u64,
    first_frame_done: bool,
}

impl AtroposApp {
    pub fn new(args: Args, create_time: u64) -> Self {
        Self {
            args,
            create_time,
            first_frame_done: false,
        }
    }

    fn on_first_frame(&mut self, ctx: &egui::Context) {
        self.first_frame_done = true;
        let ms = startup::elapsed_ms(
            self.create_time,
            atropos_core::platform::windows::now_filetime(),
        );
        tracing::debug!(first_frame_ms = ms, "first frame");
        if let Some(path) = &self.args.startup_log {
            let res = std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut f| writeln!(f, "first_frame_ms={ms:.1}"));
            if let Err(e) = res {
                tracing::warn!("startup log {}: {e}", path.display());
            }
        }
        if self.args.exit_after_first_frame {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for AtroposApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            TableBuilder::new(ui)
                .striped(true)
                .resizable(true)
                .column(Column::remainder().at_least(160.0))
                .columns(Column::auto().at_least(64.0), COLUMNS.len() - 1)
                .header(ROW_HEIGHT, |mut header| {
                    for name in COLUMNS {
                        header.col(|ui| {
                            ui.strong(name);
                        });
                    }
                })
                .body(|body| {
                    // Virtualized: only visible rows are built. Zero rows until Phase 1.
                    body.rows(ROW_HEIGHT, 0, |_row| {});
                });
        });
        if !self.first_frame_done {
            self.on_first_frame(ctx);
        }
    }
}
