//! The TUI's view model (spec Phase 7, roadmap M7.4): which rows are shown, what is selected,
//! and what each key does. No terminal I/O here, so all of it is tested directly.
//!
//! Every kill goes through the same policy check as the GUI and CLI and then asks `[y/N]`:
//! hard-blocked targets are refused with the reason, soft-blocked ones ask with a warning.

use std::sync::Arc;

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::engine::policy::{self, ActionKind, Verdict};
use atropos_core::filter::{Filter, FilterMatcher, Subject};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// What the keys are doing right now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing a search (filter syntax: `node`, `:3000`, `pid:12`, `user:me`).
    Search,
    /// Waiting for `y` / `n` on ending `key` (and its subtree when `tree`).
    Confirm {
        key: ProcKey,
        tree: bool,
        name: String,
        warning: Option<String>,
    },
}

/// What the main loop must do after a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    None,
    Quit,
    Kill { key: ProcKey, tree: bool },
}

/// One visible row.
#[derive(Debug, Clone, PartialEq)]
pub struct Row {
    pub key: ProcKey,
    pub name: String,
    pub cpu_pct: Option<f32>,
    pub memory: u64,
    pub user: String,
}

/// The whole TUI state.
pub struct Model {
    pub frame: Option<Arc<Frame>>,
    /// Atropos itself (protected by the policy).
    pub me: ProcKey,
    pub mode: Mode,
    pub search: String,
    pub rows: Vec<Row>,
    /// Index into `rows`.
    pub selected: usize,
    /// A one-line message (outcome of a kill, a refusal).
    pub message: Option<String>,
    filter: Filter,
    matcher: FilterMatcher,
}

impl Model {
    #[must_use]
    pub fn new(me: ProcKey) -> Self {
        Self {
            frame: None,
            me,
            mode: Mode::Normal,
            search: String::new(),
            rows: Vec::new(),
            selected: 0,
            message: None,
            filter: Filter::default(),
            matcher: FilterMatcher::default(),
        }
    }

    /// Takes a new frame; the selection follows its process.
    pub fn set_frame(&mut self, f: Arc<Frame>) {
        self.frame = Some(f);
        self.refresh();
    }

    /// The selected process, if any.
    #[must_use]
    pub fn selected_key(&self) -> Option<ProcKey> {
        self.rows.get(self.selected).map(|r| r.key)
    }

    /// Recomputes the rows (filter, busiest first) and keeps the selected process selected.
    fn refresh(&mut self) {
        let keep = self.selected_key();
        let Some(f) = self.frame.clone() else {
            self.rows.clear();
            return;
        };
        let ports: Option<std::collections::HashMap<u32, Vec<u16>>> =
            f.sockets.as_ref().map(|ss| {
                let mut m = std::collections::HashMap::<u32, Vec<u16>>::default();
                for s in ss.iter() {
                    m.entry(s.owner).or_default().push(s.local.port());
                }
                m
            });
        let me_user = f
            .statics
            .get(&self.me)
            .and_then(|s| s.user.ok())
            .map(ToString::to_string)
            .unwrap_or_default();
        let mut rows: Vec<Row> = f
            .snapshot
            .procs
            .iter()
            .filter(|p| {
                if self.filter.is_empty() {
                    return true;
                }
                let st = f.statics.get(&p.raw.key);
                let ctx = f.context.get(&p.raw.key);
                let subject = Subject {
                    pid: p.raw.key.pid,
                    image: &p.raw.image,
                    label: ctx.and_then(|c| c.label.as_deref()),
                    cmdline: st.and_then(|s| s.cmdline.ok()).map(|c| &**c),
                    user: st.and_then(|s| s.user.ok()).map(|u| &**u),
                    cwd: ctx.and_then(|c| c.cwd.as_deref()),
                    ports: ports
                        .as_ref()
                        .map(|m| m.get(&p.raw.key.pid).map_or(&[][..], Vec::as_slice)),
                };
                self.filter
                    .matches(&subject, &me_user, &mut self.matcher)
                    .is_some()
            })
            .map(|p| Row {
                key: p.raw.key,
                name: f
                    .context
                    .get(&p.raw.key)
                    .and_then(|c| c.label.as_deref())
                    .unwrap_or(&p.raw.image)
                    .to_owned(),
                cpu_pct: p.cpu_pct,
                memory: p.raw.private_ws,
                user: f
                    .statics
                    .get(&p.raw.key)
                    .and_then(|s| s.user.ok())
                    .map_or_else(|| "—".to_owned(), ToString::to_string),
            })
            .collect();
        rows.sort_by(|a, b| {
            b.cpu_pct
                .unwrap_or(-1.0)
                .total_cmp(&a.cpu_pct.unwrap_or(-1.0))
                .then(a.key.pid.cmp(&b.key.pid))
        });
        self.rows = rows;
        self.selected = keep
            .and_then(|k| self.rows.iter().position(|r| r.key == k))
            .unwrap_or(0)
            .min(self.rows.len().saturating_sub(1));
    }

    /// Applies one key.
    pub fn on_key(&mut self, k: KeyEvent) -> Command {
        if k.code == KeyCode::Char('c') && k.modifiers.contains(KeyModifiers::CONTROL) {
            return Command::Quit;
        }
        match self.mode.clone() {
            Mode::Search => {
                match k.code {
                    KeyCode::Esc => {
                        self.search.clear();
                        self.mode = Mode::Normal;
                    }
                    KeyCode::Enter => self.mode = Mode::Normal,
                    KeyCode::Backspace => {
                        self.search.pop();
                    }
                    KeyCode::Char(c) => self.search.push(c),
                    _ => return Command::None,
                }
                self.filter = Filter::parse(&self.search);
                self.refresh();
                Command::None
            }
            Mode::Confirm { key, tree, .. } => {
                self.mode = Mode::Normal;
                if matches!(k.code, KeyCode::Char('y' | 'Y')) {
                    return Command::Kill { key, tree };
                }
                Command::None
            }
            Mode::Normal => self.normal_key(k),
        }
    }

    fn normal_key(&mut self, k: KeyEvent) -> Command {
        match k.code {
            KeyCode::Char('q') => return Command::Quit,
            KeyCode::Down | KeyCode::Char('j') => {
                self.selected = (self.selected + 1).min(self.rows.len().saturating_sub(1));
            }
            KeyCode::Up | KeyCode::Char('k') => self.selected = self.selected.saturating_sub(1),
            KeyCode::Char('/') => {
                self.mode = Mode::Search;
                self.message = None;
            }
            KeyCode::Char('d') => self.ask(false),
            KeyCode::Char('D') => self.ask(true),
            KeyCode::Delete => self.ask(k.modifiers.contains(KeyModifiers::SHIFT)),
            _ => {}
        }
        Command::None
    }

    /// Checks the policy, then asks (soft blocks carry their warning) or refuses.
    fn ask(&mut self, tree: bool) {
        let (Some(key), Some(f)) = (self.selected_key(), self.frame.clone()) else {
            return;
        };
        let name = self.rows[self.selected].name.clone();
        let ctx = policy::Context {
            snapshot: &f.snapshot,
            statics: &f.statics,
            services: &[],
            me: self.me,
        };
        let kind = if tree {
            ActionKind::KillTree
        } else {
            ActionKind::Kill
        };
        let warning = match policy::check(&ctx, key, kind) {
            Verdict::Hard(h) => {
                self.message = Some(format!("Atropos will not end {name} ({}): {h:?}", key.pid));
                return;
            }
            Verdict::Soft(s) => Some(format!("{s:?}")),
            Verdict::Allowed => None,
        };
        self.mode = Mode::Confirm {
            key,
            tree,
            name,
            warning,
        };
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use atropos_core::Field;
    use atropos_core::platform::{RawProc, SystemStats};
    use atropos_core::ring::Ring;
    use atropos_core::snapshot::build;
    use atropos_core::statics::{Statics, StaticsMap};

    pub(crate) fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    /// 1 init → 2 node (30 % CPU) → 4 node (5 %); 3 vim (10 %); 7 is Atropos itself.
    pub(crate) fn frame() -> Frame {
        let stats = |now| SystemStats {
            logical_cpus: 1,
            total_ram: 1,
            used_ram: 1,
            now_100ns: now,
        };
        let procs = |c: [u64; 5]| {
            let mk = |pid, ppid, name: &str, cpu, ws| {
                let mut r = RawProc::new(k(pid), ppid, name);
                r.cpu_100ns = cpu;
                r.private_ws = ws;
                r
            };
            vec![
                mk(1, 0, "init", c[0], 1 << 20),
                mk(2, 1, "node", c[1], 300 << 20),
                mk(3, 1, "vim", c[2], 20 << 20),
                mk(4, 2, "node", c[3], 50 << 20),
                mk(7, 1, "atropos-tui", c[4], 8 << 20),
            ]
        };
        let first = build(None, procs([0; 5]), stats(10_000_000));
        let snapshot = build(
            Some(&first),
            procs([0, 3_000_000, 1_000_000, 500_000, 0]),
            stats(20_000_000),
        );
        let mut statics = StaticsMap::new();
        for p in &snapshot.procs {
            statics.insert(
                p.raw.key,
                Arc::new(Statics {
                    user: Field::Ok(Arc::from("josé")),
                    cmdline: Field::Ok(Arc::from(format!("{} --flag", p.raw.image).as_str())),
                    ..Statics::unavailable(&Field::Restricted)
                }),
            );
        }
        Frame {
            snapshot,
            statics: Arc::new(statics),
            cpu_history: Ring::default(),
            mem_history: Ring::default(),
            tick_ms: 0.0,
            scan_ms: 0.0,
            env: None,
            sockets: None,
            context: Arc::default(),
            history: None,
            gpu: None,
        }
    }

    pub(crate) fn model() -> Model {
        let mut m = Model::new(k(7));
        m.set_frame(Arc::new(frame()));
        m
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn pids(m: &Model) -> Vec<u32> {
        m.rows.iter().map(|r| r.key.pid).collect()
    }

    #[test]
    fn rows_are_busiest_first_and_the_cursor_moves() {
        let mut m = model();
        assert_eq!(pids(&m), vec![2, 3, 4, 1, 7]);
        assert_eq!(m.selected_key(), Some(k(2)));
        m.on_key(key(KeyCode::Down));
        m.on_key(key(KeyCode::Char('j')));
        assert_eq!(m.selected_key(), Some(k(4)));
        m.on_key(key(KeyCode::Up));
        assert_eq!(m.selected_key(), Some(k(3)));
        for _ in 0..10 {
            m.on_key(key(KeyCode::Down));
        }
        assert_eq!(m.selected_key(), Some(k(7)), "stops at the last row");
    }

    #[test]
    fn slash_searches_with_the_filter_syntax_and_esc_clears() {
        let mut m = model();
        m.on_key(key(KeyCode::Char('/')));
        assert_eq!(m.mode, Mode::Search);
        for c in "name:node".chars() {
            m.on_key(key(KeyCode::Char(c)));
        }
        assert_eq!(pids(&m), vec![2, 4], "filters while typing");
        m.on_key(key(KeyCode::Enter));
        assert_eq!(m.mode, Mode::Normal);
        assert_eq!(pids(&m), vec![2, 4], "Enter keeps the filter");
        m.on_key(key(KeyCode::Char('/')));
        m.on_key(key(KeyCode::Esc));
        assert_eq!(m.search, "");
        assert_eq!(pids(&m).len(), 5);
    }

    #[test]
    fn the_selection_follows_its_process_across_frames() {
        let mut m = model();
        m.on_key(key(KeyCode::Down)); // vim (3)
        let mut f = frame();
        // vim is now the busiest.
        let i = f.snapshot.index[&k(3)];
        f.snapshot.procs[i].cpu_pct = Some(99.0);
        m.set_frame(Arc::new(f));
        assert_eq!(m.selected_key(), Some(k(3)));
        assert_eq!(m.rows[0].key, k(3));
    }

    #[test]
    fn d_asks_and_y_ends_the_selected_process() {
        let mut m = model();
        assert_eq!(m.on_key(key(KeyCode::Char('d'))), Command::None);
        assert!(matches!(m.mode, Mode::Confirm { key, tree: false, .. } if key == k(2)));
        assert_eq!(
            m.on_key(key(KeyCode::Char('y'))),
            Command::Kill {
                key: k(2),
                tree: false
            }
        );
        assert_eq!(m.mode, Mode::Normal);
    }

    #[test]
    fn shift_delete_or_capital_d_asks_for_the_tree_and_n_cancels() {
        let mut m = model();
        m.on_key(KeyEvent::new(KeyCode::Delete, KeyModifiers::SHIFT));
        assert!(matches!(m.mode, Mode::Confirm { tree: true, .. }));
        assert_eq!(m.on_key(key(KeyCode::Char('n'))), Command::None);
        assert_eq!(m.mode, Mode::Normal);
        m.on_key(key(KeyCode::Char('D')));
        assert!(matches!(m.mode, Mode::Confirm { tree: true, .. }));
        assert_eq!(m.on_key(key(KeyCode::Esc)), Command::None, "Esc is no");
        m.on_key(key(KeyCode::Delete));
        assert!(matches!(m.mode, Mode::Confirm { tree: false, .. }));
    }

    #[test]
    fn atropos_itself_is_refused_without_asking() {
        let mut m = model();
        for _ in 0..4 {
            m.on_key(key(KeyCode::Down));
        }
        assert_eq!(m.selected_key(), Some(k(7)));
        assert_eq!(m.on_key(key(KeyCode::Char('d'))), Command::None);
        assert_eq!(m.mode, Mode::Normal);
        assert!(
            m.message
                .as_deref()
                .is_some_and(|t| t.contains("will not end"))
        );
    }

    #[test]
    fn q_quits_but_not_while_typing() {
        let mut m = model();
        m.on_key(key(KeyCode::Char('/')));
        assert_eq!(m.on_key(key(KeyCode::Char('q'))), Command::None);
        assert_eq!(m.search, "q");
        m.on_key(key(KeyCode::Esc));
        assert_eq!(m.on_key(key(KeyCode::Char('q'))), Command::Quit);
        assert_eq!(
            m.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Command::Quit
        );
    }

    #[test]
    fn an_empty_list_has_nothing_to_end() {
        let mut m = model();
        m.on_key(key(KeyCode::Char('/')));
        for c in "nothing-matches-this".chars() {
            m.on_key(key(KeyCode::Char(c)));
        }
        m.on_key(key(KeyCode::Enter));
        assert_eq!(m.selected_key(), None);
        assert_eq!(m.on_key(key(KeyCode::Char('d'))), Command::None);
        assert_eq!(m.mode, Mode::Normal);
    }
}
