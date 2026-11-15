//! Reaching Atropos from anywhere (spec Phase 6): a global hotkey (default `Ctrl+Alt+K`,
//! configurable) and a tray icon with Open / Palette / Quit. A hotkey another app already holds
//! is not an error: Atropos falls back to the tray and says so once (Z3).
//!
//! Both crates deliver their events on the UI thread's message loop (the hotkey through a hidden
//! window, the tray through its own); the handlers below only forward them and wake egui, so an
//! idle Atropos does no extra work.

use std::str::FromStr as _;

use crossbeam_channel::{Receiver, Sender};
use eframe::egui;
use global_hotkey::hotkey::{HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use tray_icon::menu::{Menu, MenuEvent, MenuItem};
use tray_icon::{MouseButton, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

use atropos_core::config::DEFAULT_HOTKEY;

/// What the hotkey or the tray asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Summon {
    /// Bring the main window back.
    Open,
    /// Bring the window back with the palette open.
    Palette,
    Quit,
}

/// Parses a hotkey such as `Ctrl+Alt+K`. It needs Ctrl, Alt or Win: a bare key or Shift+key
/// would be taken from every other program while Atropos runs.
///
/// # Errors
/// A message for the Settings window.
pub fn parse_hotkey(text: &str) -> Result<HotKey, String> {
    // "Win" is what Windows calls the key; the crate spells it "Super".
    let spelled: Vec<&str> = text
        .split('+')
        .map(|t| {
            if t.trim().eq_ignore_ascii_case("win") {
                "Super"
            } else {
                t
            }
        })
        .collect();
    let hotkey = HotKey::from_str(spelled.join("+").trim()).map_err(|_| {
        format!(
            "{:?} is not a key combination like {DEFAULT_HOTKEY}",
            text.trim()
        )
    })?;
    if !hotkey
        .mods
        .intersects(Modifiers::CONTROL | Modifiers::ALT | Modifiers::META)
    {
        return Err(format!(
            "{:?} needs Ctrl, Alt or Win, or it would be taken from every other program",
            text.trim()
        ));
    }
    Ok(hotkey)
}

/// Registers `hotkey`; on failure returns the one notice to show (the tray still works).
fn register(manager: &GlobalHotKeyManager, hotkey: HotKey, text: &str) -> Option<String> {
    match manager.register(hotkey) {
        Ok(()) => None,
        Err(global_hotkey::Error::AlreadyRegistered(_)) => Some(format!(
            "{} is already used by another program. Open the palette from the tray icon, \
             or pick another hotkey in Settings.",
            text.trim()
        )),
        Err(e) => {
            tracing::warn!("hotkey {text}: {e}");
            Some(format!(
                "Could not register {} ({e}). Open the palette from the tray icon.",
                text.trim()
            ))
        }
    }
}

/// The tray image: the exe's icon resource on Windows, the window icon's pixels elsewhere.
fn tray_image() -> Result<tray_icon::Icon, String> {
    #[cfg(windows)]
    let icon = tray_icon::Icon::from_resource(1, None);
    #[cfg(not(windows))]
    let icon = {
        let i = crate::icon::window_icon();
        tray_icon::Icon::from_rgba(i.rgba, i.width, i.height)
    };
    icon.map_err(|e| e.to_string())
}

/// The hotkey and the tray icon, alive as long as this value.
pub struct Summoner {
    manager: Option<GlobalHotKeyManager>,
    hotkey: Option<HotKey>,
    _tray: Option<TrayIcon>,
    rx: Receiver<Summon>,
}

impl Summoner {
    /// Registers `hotkey_text` and adds the tray icon. Returns the notice to show, if any. Must
    /// run on the UI thread (both need its message loop).
    pub fn start(ctx: &egui::Context, hotkey_text: &str) -> (Self, Option<String>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (open, palette, quit) = (
            MenuItem::new("Open Atropos", true, None),
            MenuItem::new(format!("Command palette\t{hotkey_text}"), true, None),
            MenuItem::new("Quit", true, None),
        );
        let ids = [
            (open.id().clone(), Summon::Open),
            (palette.id().clone(), Summon::Palette),
            (quit.id().clone(), Summon::Quit),
        ];
        install_handlers(ctx, &tx, ids);
        let menu = Menu::new();
        let tray = menu
            .append_items(&[&open, &palette, &quit])
            .map_err(|e| e.to_string())
            .and_then(|()| {
                TrayIconBuilder::new()
                    .with_tooltip("Atropos")
                    .with_icon(tray_image()?)
                    .with_menu(Box::new(menu))
                    .with_menu_on_left_click(false)
                    .build()
                    .map_err(|e| e.to_string())
            })
            .inspect_err(|e| tracing::warn!("tray icon: {e}"))
            .ok();
        let manager = GlobalHotKeyManager::new()
            .inspect_err(|e| tracing::warn!("global hotkey: {e}"))
            .ok();
        let mut me = Self {
            manager,
            hotkey: None,
            _tray: tray,
            rx,
        };
        let notice = me.rebind(hotkey_text);
        (me, notice)
    }

    /// Replaces the hotkey (after a Settings change). Returns the notice to show, if any.
    pub fn rebind(&mut self, text: &str) -> Option<String> {
        let Some(manager) = &self.manager else {
            return Some("Global hotkeys are not available; use the tray icon.".into());
        };
        if let Some(old) = self.hotkey.take() {
            let _ = manager.unregister(old);
        }
        let hotkey = match parse_hotkey(text) {
            Ok(h) => h,
            Err(e) => return Some(format!("Hotkey: {e}")),
        };
        let notice = register(manager, hotkey, text);
        if notice.is_none() {
            self.hotkey = Some(hotkey);
        }
        notice
    }

    /// The requests that arrived since the last call.
    pub fn poll(&self) -> Vec<Summon> {
        self.rx.try_iter().collect()
    }
}

/// Forwards hotkey presses, tray left-clicks and menu picks, and wakes egui. Mouse moves over
/// the tray icon are dropped here, so hovering it costs no repaint.
fn install_handlers(
    ctx: &egui::Context,
    tx: &Sender<Summon>,
    ids: [(tray_icon::menu::MenuId, Summon); 3],
) {
    let (c, t) = (ctx.clone(), tx.clone());
    GlobalHotKeyEvent::set_event_handler(Some(move |e: GlobalHotKeyEvent| {
        if e.state() == HotKeyState::Pressed {
            let _ = t.send(Summon::Palette);
            c.request_repaint();
        }
    }));
    let (c, t) = (ctx.clone(), tx.clone());
    TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
        if let TrayIconEvent::Click {
            button: MouseButton::Left,
            button_state: MouseButtonState::Up,
            ..
        } = e
        {
            let _ = t.send(Summon::Open);
            c.request_repaint();
        }
    }));
    let (c, t) = (ctx.clone(), tx.clone());
    MenuEvent::set_event_handler(Some(move |e: MenuEvent| {
        if let Some((_, s)) = ids.iter().find(|(id, _)| *id == e.id) {
            let _ = t.send(*s);
            c.request_repaint();
        }
    }));
}

#[cfg(test)]
mod tests {
    use super::*;
    use global_hotkey::hotkey::Code;

    #[test]
    fn the_default_and_lowercase_spellings_parse() {
        let want = HotKey::new(Some(Modifiers::CONTROL | Modifiers::ALT), Code::KeyK);
        assert_eq!(parse_hotkey(DEFAULT_HOTKEY), Ok(want));
        assert_eq!(parse_hotkey(" ctrl + alt + k "), Ok(want));
        assert_eq!(
            parse_hotkey("Win+Shift+F12"),
            Ok(HotKey::new(
                Some(Modifiers::META | Modifiers::SHIFT),
                Code::F12
            ))
        );
    }

    #[test]
    fn hotkeys_that_would_steal_typing_are_refused() {
        for text in ["K", "Shift+K", "", "Ctrl+", "Ctrl+Alt+Nope", "Ctrl+K+J"] {
            assert!(parse_hotkey(text).is_err(), "{text:?}");
        }
    }

    /// Registers a real (obscure) system-wide hotkey twice; nothing is left registered.
    #[test]
    fn a_hotkey_held_elsewhere_gives_one_notice() {
        let text = "Ctrl+Alt+Shift+F23";
        let hk = parse_hotkey(text).unwrap();
        let first = GlobalHotKeyManager::new().unwrap();
        let second = GlobalHotKeyManager::new().unwrap();
        assert_eq!(register(&first, hk, text), None);
        let notice = register(&second, hk, text).unwrap();
        assert!(notice.contains("Ctrl+Alt+Shift+F23"), "{notice}");
        assert!(notice.contains("tray"), "{notice}");
        first.unregister(hk).unwrap();
    }
}
