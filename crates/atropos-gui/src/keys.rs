//! Keyboard map (spec Phase 1): `Ctrl+K` / `/` search, `Enter` details, `Ctrl+C` copy row,
//! `T` tree, `F5` refresh, `?` help, `Space` freeze, `Esc` close.
//!
//! Character shortcuts (`/`, `?`, `T`) come from text events, not key codes, so they work on any
//! keyboard layout (Z4). Shortcuts that would type into the search box are ignored while it has
//! focus.

use eframe::egui::{Event, Key};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    FocusSearch,
    OpenDetails,
    CopyRow,
    ToggleTree,
    Refresh,
    ToggleHelp,
    ToggleFreeze,
    Escape,
}

/// The shortcut list shown by `?`.
pub const HELP: [(&str, &str); 8] = [
    ("Ctrl+K  or  /", "Search"),
    ("Enter", "Open details of the selected process"),
    ("Ctrl+C", "Copy the selected row"),
    ("T", "Flat list / tree"),
    ("F5", "Refresh now"),
    ("Space", "Freeze / unfreeze the row order"),
    ("Esc", "Close help or details"),
    ("?", "This help"),
];

/// Actions for this frame's input. `typing` = a text field has keyboard focus.
#[must_use]
pub fn actions(events: &[Event], typing: bool) -> Vec<Action> {
    let mut out = Vec::new();
    for e in events {
        let action = match e {
            Event::Key {
                key,
                pressed: true,
                repeat: false,
                modifiers,
                ..
            } => {
                let ctrl = modifiers.command && !modifiers.alt;
                match key {
                    Key::K if ctrl => Some(Action::FocusSearch),
                    Key::F5 => Some(Action::Refresh),
                    Key::Escape => Some(Action::Escape),
                    Key::C if ctrl && !typing => Some(Action::CopyRow),
                    Key::Enter if !typing => Some(Action::OpenDetails),
                    Key::Space if !typing => Some(Action::ToggleFreeze),
                    _ => None,
                }
            }
            Event::Text(t) if !typing => match t.as_str() {
                "/" => Some(Action::FocusSearch),
                "?" => Some(Action::ToggleHelp),
                "t" | "T" => Some(Action::ToggleTree),
                _ => None,
            },
            _ => None,
        };
        out.extend(action);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use eframe::egui::Modifiers;

    fn key(k: Key, modifiers: Modifiers) -> Event {
        Event::Key {
            key: k,
            physical_key: None,
            pressed: true,
            repeat: false,
            modifiers,
        }
    }

    fn text(s: &str) -> Event {
        Event::Text(s.into())
    }

    #[test]
    fn shortcuts_when_not_typing() {
        let cases = [
            (vec![key(Key::K, Modifiers::COMMAND)], Action::FocusSearch),
            (vec![text("/")], Action::FocusSearch),
            (vec![key(Key::Enter, Modifiers::NONE)], Action::OpenDetails),
            (vec![key(Key::C, Modifiers::COMMAND)], Action::CopyRow),
            (vec![text("t")], Action::ToggleTree),
            (vec![text("T")], Action::ToggleTree),
            (vec![key(Key::F5, Modifiers::NONE)], Action::Refresh),
            (vec![text("?")], Action::ToggleHelp),
            (
                vec![key(Key::Space, Modifiers::NONE), text(" ")],
                Action::ToggleFreeze,
            ),
            (vec![key(Key::Escape, Modifiers::NONE)], Action::Escape),
        ];
        for (events, want) in cases {
            assert_eq!(actions(&events, false), vec![want], "{events:?}");
        }
    }

    #[test]
    fn layout_independent_characters() {
        // On a German layout `/` is Shift+7 and `?` is Shift+ß: only the text matters.
        let shift7 = key(Key::Num7, Modifiers::SHIFT);
        assert_eq!(
            actions(&[shift7, text("/")], false),
            vec![Action::FocusSearch]
        );
        // A plain T key with a different text (e.g. a Cyrillic layout) is not the tree toggle.
        assert_eq!(
            actions(&[key(Key::T, Modifiers::NONE), text("е")], false),
            vec![]
        );
    }

    #[test]
    fn typing_in_search_keeps_characters_and_editing_keys() {
        for events in [
            vec![text("/")],
            vec![text("t")],
            vec![text("?")],
            vec![key(Key::Space, Modifiers::NONE), text(" ")],
            vec![key(Key::Enter, Modifiers::NONE)],
            vec![key(Key::C, Modifiers::COMMAND)],
        ] {
            assert_eq!(actions(&events, true), vec![], "{events:?}");
        }
        // These still work while typing.
        assert_eq!(
            actions(&[key(Key::K, Modifiers::COMMAND)], true),
            vec![Action::FocusSearch]
        );
        assert_eq!(
            actions(&[key(Key::F5, Modifiers::NONE)], true),
            vec![Action::Refresh]
        );
        assert_eq!(
            actions(&[key(Key::Escape, Modifiers::NONE)], true),
            vec![Action::Escape]
        );
    }

    #[test]
    fn key_release_and_repeat_do_nothing() {
        let released = Event::Key {
            key: Key::F5,
            physical_key: None,
            pressed: false,
            repeat: false,
            modifiers: Modifiers::NONE,
        };
        let repeat = Event::Key {
            key: Key::F5,
            physical_key: None,
            pressed: true,
            repeat: true,
            modifiers: Modifiers::NONE,
        };
        assert_eq!(actions(&[released, repeat], false), vec![]);
    }
}
