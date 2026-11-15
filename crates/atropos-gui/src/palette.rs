//! The command palette (spec Phase 6): summoned by the global hotkey or the tray icon.
//! `3000 ⏎` ends what holds port 3000; `node`, `pid:12` or any search syntax lists matches;
//! `lock <path>` lists what has a file or folder open. Every kill goes through the usual
//! `Actions` flow (policy, confirmation dialog, respawn warning).

use std::collections::HashMap;
use std::path::PathBuf;

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::filter::{Filter, FilterMatcher};
use atropos_core::ports::{Proto, TcpState};

use crate::view;
use eframe::egui::{self, Key, Modifiers};

/// The most matches the palette lists.
pub const MAX_RESULTS: usize = 8;

/// What the palette text asks for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteCmd {
    Empty,
    /// `3000` or `:3000`: what owns that local port.
    Port(u16),
    /// Any search-bar syntax: `node`, `pid:12`, `name:node cwd:shop`.
    Query(String),
    /// `lock <path>`: what has this file or folder open.
    Lock(PathBuf),
}

/// Parses the palette text. Never fails; a leading `kill` is accepted and changes nothing
/// (Enter ends the chosen match either way).
#[must_use]
pub fn parse(input: &str) -> PaletteCmd {
    let s = input.trim();
    if let Some(rest) = verb(s, "kill") {
        return parse(rest);
    }
    if let Some(rest) = verb(s, "lock") {
        let path = rest.trim().trim_matches('"');
        return if path.is_empty() {
            PaletteCmd::Empty
        } else {
            PaletteCmd::Lock(PathBuf::from(path))
        };
    }
    if s.is_empty() {
        return PaletteCmd::Empty;
    }
    let digits = s.strip_prefix(':').unwrap_or(s);
    match digits.parse::<u16>() {
        Ok(port) if port != 0 && digits.bytes().all(|b| b.is_ascii_digit()) => {
            PaletteCmd::Port(port)
        }
        _ => PaletteCmd::Query(s.to_owned()),
    }
}

/// The text after a leading `word` (case-insensitive) when it is the whole text or followed by
/// whitespace.
fn verb<'a>(s: &'a str, word: &str) -> Option<&'a str> {
    let head = s.get(..word.len())?;
    let rest = &s[word.len()..];
    (head.eq_ignore_ascii_case(word) && (rest.is_empty() || rest.starts_with(char::is_whitespace)))
        .then_some(rest)
}

/// The processes `cmd` points at, best first, at most [`MAX_RESULTS`]. `lockers` is the last
/// file-lock lookup (used by [`PaletteCmd::Lock`]).
#[must_use]
pub fn matches(
    frame: &Frame,
    cmd: &PaletteCmd,
    me: &str,
    m: &mut FilterMatcher,
    lockers: &[ProcKey],
) -> Vec<ProcKey> {
    let snap = &frame.snapshot;
    let mut out: Vec<ProcKey> = match cmd {
        PaletteCmd::Empty => Vec::new(),
        PaletteCmd::Port(port) => {
            let Some(sockets) = &frame.sockets else {
                return Vec::new();
            };
            // (holds it: listening TCP or bound UDP, PID) for each owner present in the snapshot.
            let mut owners: Vec<(bool, ProcKey)> = Vec::new();
            for s in sockets.iter().filter(|s| s.local.port() == *port) {
                let holds = s.proto == Proto::Udp || s.state == Some(TcpState::Listen);
                let Some(p) = snap.procs.iter().find(|p| p.raw.key.pid == s.owner) else {
                    continue;
                };
                match owners.iter_mut().find(|(_, k)| *k == p.raw.key) {
                    Some(o) => o.0 |= holds,
                    None => owners.push((holds, p.raw.key)),
                }
            }
            owners.sort_by_key(|&(holds, k)| (!holds, k.pid));
            owners.into_iter().map(|(_, k)| k).collect()
        }
        PaletteCmd::Query(q) => {
            let filter = Filter::parse(q);
            let ports = filter.has_port_term().then(|| port_map(frame)).flatten();
            let mut scored: Vec<(u32, ProcKey)> = snap
                .procs
                .iter()
                .filter_map(|p| {
                    let st = frame.statics.get(&p.raw.key).map(|s| &**s);
                    let own = ports
                        .as_ref()
                        .map(|m| m.get(&p.raw.key.pid).map_or(&[][..], Vec::as_slice));
                    let ctx = frame.context.get(&p.raw.key);
                    filter
                        .matches(&view::subject(p, st, ctx, own), me, m)
                        .map(|score| (score, p.raw.key))
                })
                .collect();
            scored.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.pid.cmp(&b.1.pid)));
            scored.into_iter().map(|(_, k)| k).collect()
        }
        PaletteCmd::Lock(_) => lockers
            .iter()
            .copied()
            .filter(|k| snap.get(*k).is_some())
            .collect(),
    };
    out.truncate(MAX_RESULTS);
    out
}

/// Local ports per PID (for `:port` terms inside a query).
fn port_map(frame: &Frame) -> Option<HashMap<u32, Vec<u16>>> {
    frame.sockets.as_ref().map(|ss| {
        let mut m: HashMap<u32, Vec<u16>> = HashMap::new();
        for s in ss.iter() {
            m.entry(s.owner).or_default().push(s.local.port());
        }
        m
    })
}

/// What the user did in the palette this frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PaletteEvent {
    /// Enter: end this process (through the usual confirmation flow).
    Kill(ProcKey),
    /// Shift+Enter: select it in the main window.
    Show(ProcKey),
    /// Enter on `lock <path>` before a lookup: find what has it open.
    Lookup(PathBuf),
}

/// A file-lock lookup: the path and its lockers, or a message.
pub type LockAnswer = (PathBuf, Result<Vec<ProcKey>, String>);
/// The last lookup, if any.
pub type LockResult = Option<LockAnswer>;

/// Palette window state.
#[derive(Debug, Default)]
pub struct Palette {
    pub open: bool,
    text: String,
    /// The highlighted match; `None` until the user picks one (↑ ↓ or a click).
    chosen: Option<usize>,
    focus: bool,
    pub lock: LockResult,
    /// A lookup that is running for this path.
    pub looking: Option<PathBuf>,
}

impl Palette {
    /// Opens the palette empty, with the cursor in the text field.
    pub fn summon(&mut self) {
        *self = Self {
            open: true,
            focus: true,
            ..Self::default()
        };
    }

    /// Opens the palette with `text` already typed (the cursor at its end).
    pub fn summon_with(&mut self, text: &str) {
        self.summon();
        text.clone_into(&mut self.text);
    }

    /// Whether the collector should read sockets for the current text.
    #[must_use]
    pub fn wants_ports(&self) -> bool {
        self.open
            && match parse(&self.text) {
                PaletteCmd::Port(_) => true,
                PaletteCmd::Query(q) => Filter::parse(&q).has_port_term(),
                PaletteCmd::Empty | PaletteCmd::Lock(_) => false,
            }
    }
}

const HINT: &str = r"3000   node   pid:12   lock C:\path\file";

/// Draws the palette (when open); returns what the user asked for.
pub fn show(
    ctx: &egui::Context,
    st: &mut Palette,
    frame: &Frame,
    me: &str,
    m: &mut FilterMatcher,
) -> Option<PaletteEvent> {
    if !st.open {
        return None;
    }
    let cmd = parse(&st.text);
    let looked_up = matches!((&cmd, &st.lock), (PaletteCmd::Lock(p), Some((q, _))) if p == q);
    let lockers: &[ProcKey] = match (&st.lock, looked_up) {
        (Some((_, Ok(keys))), true) => keys,
        _ => &[],
    };
    let found = matches(frame, &cmd, me, m, lockers);
    // The palette owns the keyboard: take its keys before the text field sees them.
    let (esc, show_it, enter, up, down) = ctx.input_mut(|i| {
        (
            i.consume_key(Modifiers::NONE, Key::Escape),
            i.consume_key(Modifiers::SHIFT, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::Enter),
            i.consume_key(Modifiers::NONE, Key::ArrowUp),
            i.consume_key(Modifiers::NONE, Key::ArrowDown),
        )
    });
    if esc {
        st.open = false;
        return None;
    }
    if let Some(last) = found.len().checked_sub(1) {
        if down {
            st.chosen = Some(st.chosen.map_or(0, |c| (c + 1).min(last)));
        }
        if up {
            st.chosen = Some(st.chosen.map_or(0, |c| c.saturating_sub(1)));
        }
        st.chosen = st.chosen.map(|c| c.min(last));
    }
    let target = target(&found, st.chosen);
    let event = if show_it || enter {
        match (&cmd, target) {
            (PaletteCmd::Lock(p), _) if !looked_up => {
                (st.looking.as_ref() != Some(p)).then(|| PaletteEvent::Lookup(p.clone()))
            }
            (_, Some(k)) if show_it => Some(PaletteEvent::Show(k)),
            (_, Some(k)) => Some(PaletteEvent::Kill(k)),
            _ => None,
        }
    } else {
        None
    };
    let status = match &cmd {
        PaletteCmd::Lock(p) if !looked_up && st.looking.as_ref() == Some(p) => {
            "Looking for what has it open…".to_owned()
        }
        _ => status(frame, &cmd, &st.lock, looked_up, &found, target),
    };
    egui::Window::new("Command palette")
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::CENTER_TOP, [0.0, 48.0])
        .fixed_size([520.0, 0.0])
        .show(ctx, |ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut st.text)
                    .hint_text(HINT)
                    .desired_width(f32::INFINITY)
                    .font(egui::TextStyle::Heading),
            );
            if std::mem::take(&mut st.focus) {
                // Prefilled text: put the cursor at its end.
                if let Some(mut state) = egui::TextEdit::load_state(ui.ctx(), r.id) {
                    let end = egui::text::CCursor::new(st.text.chars().count());
                    state
                        .cursor
                        .set_char_range(Some(egui::text::CCursorRange::one(end)));
                    state.store(ui.ctx(), r.id);
                }
                r.request_focus();
            } else if !r.has_focus() {
                r.request_focus();
            }
            if r.changed() {
                st.chosen = None;
            }
            for (i, key) in found.iter().enumerate() {
                if ui
                    .selectable_label(target == Some(*key), line(frame, *key))
                    .clicked()
                {
                    st.chosen = Some(i);
                }
            }
            ui.label(egui::RichText::new(status).weak());
            ui.label(
                egui::RichText::new("Enter ends it · Shift+Enter shows it · Esc closes")
                    .weak()
                    .small(),
            );
        });
    if matches!(event, Some(PaletteEvent::Kill(_) | PaletteEvent::Show(_))) {
        st.open = false;
    }
    event
}

/// What Enter acts on: the only match, or the one the user picked (nothing is ended implicitly
/// when there are several).
fn target(found: &[ProcKey], chosen: Option<usize>) -> Option<ProcKey> {
    match (found, chosen) {
        ([only], _) => Some(*only),
        (_, Some(c)) => found.get(c).copied(),
        _ => None,
    }
}

/// One result line: name, PID and the start of the command line.
fn line(frame: &Frame, key: ProcKey) -> String {
    let Some(p) = frame.snapshot.get(key) else {
        return format!("PID {}", key.pid);
    };
    let cmd: String = frame
        .statics
        .get(&key)
        .and_then(|s| s.cmdline.ok())
        .map(|c| c.chars().take(60).collect())
        .unwrap_or_default();
    format!("{}  ({})  {cmd}", view::display_name(frame, p), key.pid)
}

/// The line under the results: what Enter will do, or why there is nothing.
fn status(
    frame: &Frame,
    cmd: &PaletteCmd,
    lock: &LockResult,
    looked_up: bool,
    found: &[ProcKey],
    target: Option<ProcKey>,
) -> String {
    match (cmd, lock) {
        (PaletteCmd::Empty, _) => {
            "Type a port, a name or a search; lock <path> for file locks.".into()
        }
        (PaletteCmd::Lock(_), _) if !looked_up => "Enter: find what has it open.".into(),
        (PaletteCmd::Lock(_), Some((_, Err(e)))) => e.clone(),
        (PaletteCmd::Lock(_), _) if found.is_empty() => "Nothing has it open.".into(),
        (PaletteCmd::Port(port), _) if found.is_empty() => {
            if frame.sockets.is_some() {
                format!("Nothing holds port {port}.")
            } else {
                "Reading ports…".into()
            }
        }
        _ if found.is_empty() => "No match.".into(),
        _ => match target {
            Some(k) => format!("Enter ends PID {}.", k.pid),
            None => format!("{} matches: pick one with ↑ ↓ or a click.", found.len()),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, k};
    use atropos_core::ports::Socket;
    use std::sync::Arc;

    #[test]
    fn a_number_or_colon_number_is_a_port() {
        assert_eq!(parse("3000"), PaletteCmd::Port(3000));
        assert_eq!(parse(" :3000 "), PaletteCmd::Port(3000));
        assert_eq!(parse("kill 3000"), PaletteCmd::Port(3000));
        assert_eq!(parse("KILL :65535"), PaletteCmd::Port(65535));
    }

    #[test]
    fn numbers_that_are_not_ports_are_searches() {
        assert_eq!(parse("0"), PaletteCmd::Query("0".into()));
        assert_eq!(parse("70000"), PaletteCmd::Query("70000".into()));
        assert_eq!(parse(":abc"), PaletteCmd::Query(":abc".into()));
    }

    #[test]
    fn words_are_searches_and_kill_is_optional() {
        assert_eq!(parse("node"), PaletteCmd::Query("node".into()));
        assert_eq!(parse("kill node"), PaletteCmd::Query("node".into()));
        assert_eq!(
            parse("  name:node   cwd:shop "),
            PaletteCmd::Query("name:node   cwd:shop".into())
        );
        // "killer" is a word, not the verb.
        assert_eq!(parse("killer"), PaletteCmd::Query("killer".into()));
    }

    #[test]
    fn lock_takes_the_rest_as_a_path_without_quotes() {
        assert_eq!(
            parse(r#"lock "C:\Tëmp dir\a b.txt""#),
            PaletteCmd::Lock(PathBuf::from(r"C:\Tëmp dir\a b.txt"))
        );
        assert_eq!(
            parse(r"Lock \\?\D:\very\long"),
            PaletteCmd::Lock(PathBuf::from(r"\\?\D:\very\long"))
        );
    }

    #[test]
    fn empty_and_bare_verbs_ask_for_nothing() {
        for s in ["", "   ", "kill", "kill  ", "lock", "lock \"\""] {
            assert_eq!(parse(s), PaletteCmd::Empty, "{s:?}");
        }
    }

    fn sock(port: u16, owner: u32, proto: Proto, state: Option<TcpState>) -> Socket {
        Socket {
            proto,
            local: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            remote: None,
            state,
            owner,
        }
    }

    #[test]
    fn port_matches_list_listeners_first() {
        let mut f = frame();
        f.sockets = Some(Arc::new(vec![
            // 3 has an outgoing connection from local port 3000; 4 and 2 listen on it
            // (IPv4 + IPv6, a parent/child pair).
            sock(3000, 3, Proto::Tcp, Some(TcpState::Established)),
            sock(3000, 4, Proto::Tcp, Some(TcpState::Listen)),
            sock(3000, 2, Proto::Tcp, Some(TcpState::Listen)),
            sock(5000, 9, Proto::Udp, None),
        ]));
        let got = matches(
            &f,
            &PaletteCmd::Port(3000),
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(got, vec![k(2), k(4), k(3)]);
        let udp = matches(
            &f,
            &PaletteCmd::Port(5000),
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(udp, vec![k(9)]);
    }

    #[test]
    fn port_matches_are_empty_until_sockets_are_read() {
        let got = matches(
            &frame(),
            &PaletteCmd::Port(3000),
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(got, Vec::new());
    }

    #[test]
    fn queries_rank_by_score_then_pid() {
        let got = matches(
            &frame(),
            &PaletteCmd::Query("node".into()),
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(got, vec![k(2), k(4)]);
        let pid = matches(
            &frame(),
            &PaletteCmd::Query("pid:9".into()),
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(pid, vec![k(9)]);
    }

    #[test]
    fn lock_matches_are_the_lockers_still_running() {
        let gone = ProcKey {
            pid: 77,
            create_time: 1,
        };
        let got = matches(
            &frame(),
            &PaletteCmd::Lock(PathBuf::from("x")),
            "",
            &mut FilterMatcher::default(),
            &[k(3), gone, k(1)],
        );
        assert_eq!(got, vec![k(3), k(1)]);
    }

    #[test]
    fn empty_lists_nothing_and_results_are_capped() {
        let none = matches(
            &frame(),
            &PaletteCmd::Empty,
            "",
            &mut FilterMatcher::default(),
            &[],
        );
        assert_eq!(none, Vec::new());
        let many: Vec<ProcKey> = (0..20).map(|_| k(1)).collect();
        let capped = matches(
            &frame(),
            &PaletteCmd::Lock(PathBuf::from("x")),
            "",
            &mut FilterMatcher::default(),
            &many,
        );
        assert_eq!(capped.len(), MAX_RESULTS);
    }

    struct S {
        p: Palette,
        frame: Frame,
        events: Vec<PaletteEvent>,
    }

    fn harness(frame: Frame) -> egui_kittest::Harness<'static, S> {
        let mut p = Palette::default();
        p.summon();
        egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 500.0))
            .build_state(
                |ctx, s: &mut S| {
                    let mut m = FilterMatcher::default();
                    if let Some(e) = show(ctx, &mut s.p, &s.frame, "", &mut m) {
                        s.events.push(e);
                    }
                },
                S {
                    p,
                    frame,
                    events: Vec::new(),
                },
            )
    }

    fn typed(h: &mut egui_kittest::Harness<'static, S>, text: &str) {
        h.run();
        h.input_mut()
            .events
            .push(egui::Event::Text(text.to_owned()));
        h.run();
    }

    fn holders(owners: &[u32]) -> Frame {
        let mut f = frame();
        f.sockets = Some(Arc::new(
            owners
                .iter()
                .map(|&o| sock(3000, o, Proto::Tcp, Some(TcpState::Listen)))
                .collect(),
        ));
        f
    }

    #[test]
    fn port_then_enter_ends_its_holder_and_closes() {
        let mut h = harness(holders(&[2]));
        typed(&mut h, "3000");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().events, vec![PaletteEvent::Kill(k(2))]);
        assert!(!h.state().p.open);
    }

    #[test]
    fn two_holders_need_an_explicit_pick() {
        let mut h = harness(holders(&[2, 4]));
        typed(&mut h, "3000");
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().events, Vec::new());
        assert!(h.state().p.open);
        h.key_press(Key::ArrowDown);
        h.run();
        h.key_press(Key::ArrowDown);
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().events, vec![PaletteEvent::Kill(k(4))]);
    }

    #[test]
    fn shift_enter_shows_instead_of_ending() {
        let mut h = harness(frame());
        typed(&mut h, "pid:3");
        h.key_press_modifiers(Modifiers::SHIFT, Key::Enter);
        h.run();
        assert_eq!(h.state().events, vec![PaletteEvent::Show(k(3))]);
    }

    #[test]
    fn escape_closes_without_acting() {
        let mut h = harness(holders(&[2]));
        typed(&mut h, "3000");
        h.key_press(Key::Escape);
        h.run();
        assert!(!h.state().p.open);
        assert_eq!(h.state().events, Vec::new());
    }

    #[test]
    fn lock_asks_for_a_lookup_then_ends_the_chosen_locker() {
        let mut h = harness(frame());
        typed(&mut h, r"lock C:\a b\f.txt");
        h.key_press(Key::Enter);
        h.run();
        let path = PathBuf::from(r"C:\a b\f.txt");
        assert_eq!(h.state().events, vec![PaletteEvent::Lookup(path.clone())]);
        assert!(h.state().p.open, "stays open to show the lockers");
        h.state_mut().p.lock = Some((path, Ok(vec![k(3)])));
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().events.last(), Some(&PaletteEvent::Kill(k(3))));
    }

    #[test]
    fn ports_are_wanted_only_for_port_text() {
        let mut p = Palette::default();
        assert!(!p.wants_ports());
        p.summon();
        p.text = "3000".into();
        assert!(p.wants_ports());
        p.text = "node :5173".into();
        assert!(p.wants_ports());
        p.text = "node".into();
        assert!(!p.wants_ports());
    }

    #[test]
    fn a_prefilled_lock_shows_looking_and_does_not_ask_twice() {
        use egui_kittest::kittest::Queryable as _;
        let mut p = Palette::default();
        p.summon_with(r#"lock "C:\a b\f.txt""#);
        p.looking = Some(PathBuf::from(r"C:\a b\f.txt"));
        let mut h = egui_kittest::Harness::builder()
            .with_size(egui::vec2(800.0, 500.0))
            .build_state(
                |ctx, s: &mut S| {
                    let mut m = FilterMatcher::default();
                    if let Some(e) = show(ctx, &mut s.p, &s.frame, "", &mut m) {
                        s.events.push(e);
                    }
                },
                S {
                    p,
                    frame: frame(),
                    events: Vec::new(),
                },
            );
        h.run();
        h.key_press(Key::Enter);
        h.run();
        assert_eq!(h.state().events, Vec::new());
        assert!(h.query_by_label_contains("Looking").is_some());
    }
}
