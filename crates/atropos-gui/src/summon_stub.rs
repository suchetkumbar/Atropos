//! Linux stand-in for `summon.rs`: no tray icon and no global hotkey. The `tray-icon` crate
//! needs GTK and `AppIndicator` system libraries there, which a zero-dependency Atropos does not
//! ship (Z1); the palette opens from the window ("What is locking…") instead.

#![allow(clippy::unused_self)] // the same API as `summon.rs`

use eframe::egui;

/// What the hotkey or the tray asked for (never produced here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)] // the app matches on every variant
pub enum Summon {
    Open,
    Palette,
    Quit,
}

/// Hotkeys are not available on this OS.
///
/// # Errors
/// Always.
pub fn parse_hotkey(_text: &str) -> Result<(), String> {
    Err("Global hotkeys are not available on Linux.".into())
}

/// Nothing to hold.
pub struct Summoner;

impl Summoner {
    pub fn start(_ctx: &egui::Context, _hotkey_text: &str) -> (Self, Option<String>) {
        (Self, None)
    }

    pub fn rebind(&mut self, _text: &str) -> Option<String> {
        None
    }

    pub fn poll(&self) -> Vec<Summon> {
        Vec::new()
    }
}
