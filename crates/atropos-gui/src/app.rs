//! The main window: search bar and toggles on top, the process table below. Repaints only when
//! the collector delivers a frame or the user interacts (no `request_repaint` loop).

use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use atropos_core::ProcKey;
use atropos_core::collector::{self, Collector, Frame, Interest, Visibility};
use atropos_core::filter::{Filter, FilterMatcher};
use atropos_core::platform::windows::{
    WindowsPlatform, current_process_key, explorer_menu, local_time, now_filetime,
};
use eframe::egui;

use crate::actions::{Act, Actions};
use crate::args::Args;
use crate::blast::{self, BlastEvent, BlastState};
use crate::details::{self, DetailEvent, DetailsState};
use crate::header;
use crate::keys::{self, Action};
use crate::palette::{self, Palette, PaletteEvent};
use crate::projects_bar::{self, BarEvent};
use crate::selection::{OrderFreeze, Selection};
use crate::settings::{self, SettingsUi};
use crate::startup;
use crate::summon::{Summon, Summoner};
use crate::table::{self, Event, Look};
use crate::view::{self, Row, Scope, Sort, SortKey, ViewOpts};
use atropos_core::config::{self, Config, View};

const SEARCH_HINT: &str = "Search   :3000   pid:123   user:me   name:node   cwd:shop";

/// Keyboard-driven UI state.
#[derive(Debug, Default)]
struct KeyState {
    help: bool,
    focus_search: bool,
}

pub struct AtroposApp {
    args: Args,
    create_time: u64,
    first_frame_done: bool,
    collector: Option<Collector>,
    sent: Option<Interest>,
    frame: Option<Arc<Frame>>,
    self_key: Option<ProcKey>,
    /// Our own account name, for `user:me` (read from our own statics).
    me: Option<String>,
    search: String,
    filter: Filter,
    matcher: FilterMatcher,
    opts: ViewOpts,
    selection: Selection,
    details: DetailsState,
    keys: KeyState,
    freeze: OrderFreeze,
    rows: Vec<Row>,
    dirty: bool,
    pointer_on_table: bool,
    perf_log: Option<std::fs::File>,
    actions: Actions,
    config: Config,
    settings: SettingsUi,
    /// Processes a crashed previous run left suspended (asked once at start).
    resume_offer: Option<crate::crash::ResumeOffer>,
    /// Blast Radius zoom and layout.
    blast: BlastState,
    /// The command palette (hotkey / tray).
    palette: Palette,
    /// The global hotkey and the tray icon (`None` in a `--lock` window from Explorer, which
    /// is a one-off lookup, not a second resident Atropos).
    summoner: Option<Summoner>,
    /// The hotkey text `summoner` was last bound with.
    bound_hotkey: String,
    /// The "Export snapshot" window.
    export: crate::export_ui::ExportUi,
    /// The "Set affinity" window, when open.
    affinity: Option<crate::affinity::AffinityUi>,
    /// The History window (kill history + Restart).
    history: crate::history_view::HistoryUi,
    /// The running file-lock lookup, if any.
    lock_rx: Option<crossbeam_channel::Receiver<palette::LockAnswer>>,
}

impl AtroposApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args, create_time: u64) -> Self {
        let ctx = cc.egui_ctx.clone();
        let (config, notice) = config::load(&config::path());
        let collector = collector::spawn_with(
            Box::new(WindowsPlatform::new()),
            // A non-zero delay paints once; `request_repaint()` always paints twice.
            Box::new(move || ctx.request_repaint_after(std::time::Duration::from_millis(1))),
            config.labels.clone(),
        )
        .inspect_err(|e| tracing::error!("collector thread: {e}"))
        .ok();
        let perf_log = args.perf_log.as_ref().and_then(|p| {
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(p)
                .inspect_err(|e| tracing::warn!("perf log {}: {e}", p.display()))
                .ok()
        });
        let self_key = current_process_key().ok();
        let mut actions = Actions::spawn(&cc.egui_ctx, self_key);
        if let Some(n) = notice {
            tracing::warn!("{n}");
            actions.notice(n, Instant::now());
        }
        // A broken label rule is reported once and skipped; the others still apply (Z3).
        let bad_rules = atropos_core::labels::check_user(&config.labels);
        if !bad_rules.is_empty() {
            let n = format!("config.toml: {}", bad_rules.join("; "));
            tracing::warn!("{n}");
            actions.notice(n, Instant::now());
        }
        let summoner = args.lock.is_none().then(|| {
            let (s, notice) = Summoner::start(&cc.egui_ctx, &config.hotkey);
            if let Some(n) = notice {
                actions.notice(n, Instant::now());
            }
            s
        });
        cc.egui_ctx.set_theme(settings::theme(config.theme));
        actions.grace = std::time::Duration::from_secs(u64::from(config.grace_secs));
        actions.confirm = config.confirm;
        let leftovers = actions.leftover_suspends();
        let resume_offer = (!leftovers.is_empty()).then(|| {
            let mut raw = Vec::new();
            let _ = atropos_core::platform::Platform::scan(&mut WindowsPlatform::new(), &mut raw);
            crate::crash::ResumeOffer {
                items: leftovers
                    .iter()
                    .map(|(k, n)| {
                        let name = raw
                            .iter()
                            .find(|r| r.key == *k)
                            .map_or_else(|| "process".to_owned(), |r| r.image.to_string());
                        (*k, *n, name)
                    })
                    .collect(),
            }
        });
        let opts = ViewOpts {
            tree: config.default_view == View::Tree,
            ..ViewOpts::default()
        };
        Self {
            args,
            create_time,
            first_frame_done: false,
            collector,
            sent: None,
            frame: None,
            self_key,
            me: None,
            search: String::new(),
            filter: Filter::default(),
            matcher: FilterMatcher::default(),
            opts,
            selection: Selection::default(),
            details: DetailsState::default(),
            keys: KeyState::default(),
            freeze: OrderFreeze::default(),
            rows: Vec::new(),
            dirty: false,
            pointer_on_table: false,
            perf_log,
            actions,
            settings: SettingsUi::default(),
            resume_offer,
            blast: BlastState::default(),
            palette: Palette::default(),
            summoner,
            bound_hotkey: config.hotkey.clone(),
            config,
            lock_rx: None,
            history: crate::history_view::HistoryUi::default(),
            affinity: None,
            export: crate::export_ui::ExportUi::default(),
        }
        .with_lock_request(&cc.egui_ctx)
    }

    /// Takes the newest frame, if the collector sent one.
    fn poll(&mut self) {
        let Some(c) = &self.collector else {
            return;
        };
        let Some(f) = c.frames.try_iter().last() else {
            return;
        };
        if let Some(log) = &mut self.perf_log {
            let _ = writeln!(log, "tick_ms={:.3} scan_ms={:.3}", f.tick_ms, f.scan_ms);
        }
        self.selection.retain(&f.snapshot);
        let had_cpu = self
            .frame
            .as_ref()
            .is_some_and(|old| old.snapshot.total_cpu_pct().is_some());
        if !had_cpu && f.snapshot.total_cpu_pct().is_some() {
            // The first frame had no CPU values to sort by; sort now even under the pointer.
            self.freeze.invalidate();
        }
        if self.me.is_none() {
            self.me = self
                .self_key
                .and_then(|k| f.statics.get(&k))
                .and_then(|s| s.user.ok())
                .map(ToString::to_string);
        }
        self.frame = Some(f);
        self.dirty = true;
    }

    /// Tells the collector how visible we are (adaptive tick) and what is selected.
    fn steer(&mut self, ctx: &egui::Context, refresh_now: bool) {
        let visibility = ctx.input(|i| {
            let v = i.viewport();
            if v.minimized == Some(true) {
                Visibility::Minimized
            } else if v.focused == Some(false) {
                Visibility::Visible
            } else {
                Visibility::Focused
            }
        });
        let want = Interest {
            visibility,
            selected: self.selection.0,
            tick_override: self
                .config
                .tick_secs
                .map(|s| std::time::Duration::from_secs_f32(s.clamp(0.5, 10.0))),
            want_ports: self.filter.has_port_term()
                || (self.details.open && self.details.tab == details::Tab::Ports)
                || self.palette.wants_ports(),
            want_projects: self.opts.scope == Scope::Projects,
            want_leftovers: self.opts.scope == Scope::Leftovers,
            want_gpu: if self.opts.scope == Scope::Blast {
                self.blast.measure == blast::Measure::Gpu
            } else {
                self.opts.gpu
            },
            ..Interest::default()
        };
        if let Some(c) = &self.collector {
            if refresh_now {
                let _ = c.interest.send(Interest {
                    refresh_now: true,
                    ..want
                });
            } else if self.sent != Some(want) {
                let _ = c.interest.send(want);
            }
        }
        self.sent = Some(want);
    }

    fn changed_view(&mut self) {
        self.freeze.invalidate();
        self.dirty = true;
    }

    fn recompute(&mut self) {
        if !self.dirty {
            return;
        }
        let Some(f) = &self.frame else {
            return;
        };
        let fresh = view::rows(
            f,
            &self.filter,
            self.me.as_deref().unwrap_or(""),
            &self.opts,
            &mut self.matcher,
        );
        // The order holds still under the pointer and while a dialog is open.
        let hold = self.pointer_on_table || self.keys.help || self.actions.holds();
        self.rows = self.freeze.apply(fresh, |r| r.key, hold, Instant::now());
        self.dirty = false;
    }

    fn on_event(&mut self, e: Event) {
        match e {
            Event::Select(key) => {
                self.selection.0 = Some(key);
                self.details.open = true;
            }
            Event::Sort(key) => {
                let descending = if self.opts.sort.key == key {
                    !self.opts.sort.descending
                } else {
                    // Text columns start A→Z; numbers start with the largest.
                    !matches!(key, SortKey::Name | SortKey::User)
                };
                self.opts.sort = Sort { key, descending };
                self.changed_view();
            }
            Event::Toggle(key) => {
                if !self.opts.collapsed.remove(&key) {
                    self.opts.collapsed.insert(key);
                }
                self.changed_view();
            }
            Event::Act(key, act) => self.act(key, act),
            Event::Tune(key, t) => {
                let _ = self.tune(key, t);
            }
            Event::Affinity(key) => self.open_affinity(key),
        }
    }

    fn act(&mut self, key: ProcKey, act: Act) {
        if let Some(f) = &self.frame {
            self.actions.start(key, act, f);
        }
    }

    /// Priority / affinity / efficiency mode through a pin, after the policy check.
    fn tune(&mut self, key: ProcKey, t: atropos_core::tune::Tune) -> Result<(), String> {
        let Some(f) = self.frame.clone() else {
            return Err("No process list yet.".into());
        };
        let ctx = atropos_core::engine::policy::Context {
            snapshot: &f.snapshot,
            statics: &f.statics,
            services: &[],
            me: self.self_key.unwrap_or(ProcKey {
                pid: 0,
                create_time: 0,
            }),
        };
        let name = f
            .snapshot
            .get(key)
            .map_or_else(|| "process".to_owned(), |p| p.raw.image.to_string());
        let now = Instant::now();
        let r = atropos_core::tune::apply(&mut WindowsPlatform::new(), &ctx, key, t);
        match &r {
            Ok(()) => {
                let what = match t {
                    atropos_core::tune::Tune::Priority(p) => format!("priority {}", p.text()),
                    atropos_core::tune::Tune::Affinity(m) => {
                        format!("CPUs {}", atropos_core::tune::format_affinity(m))
                    }
                    atropos_core::tune::Tune::Eco(true) => "efficiency mode on".to_owned(),
                    atropos_core::tune::Tune::Eco(false) => "efficiency mode off".to_owned(),
                };
                self.actions
                    .info(format!("{name} ({}): {what}", key.pid), now);
            }
            Err(e) => self
                .actions
                .notice(format!("{name} ({}): {e}", key.pid), now),
        }
        r
    }

    /// Writes the current frame as JSON into the export folder.
    fn export_snapshot(
        &self,
        unmasked: bool,
        today: Option<atropos_core::platform::windows::LocalTime>,
    ) -> Result<std::path::PathBuf, String> {
        let f = self.frame.as_ref().ok_or("No process list yet.")?;
        // The frame carries sockets only while a port view is open: read them now otherwise.
        let sockets = match &f.sockets {
            Some(s) => Some(s.to_vec()),
            None => atropos_core::platform::Platform::sockets(&mut WindowsPlatform::new()).ok(),
        };
        let doc = atropos_core::export::build(
            &f.snapshot,
            &f.statics,
            &f.context,
            sockets.as_deref(),
            !unmasked,
            now_filetime(),
        );
        let dir = crate::export_ui::folder(|k| std::env::var_os(k), std::path::Path::is_dir);
        let path = dir.join(crate::export_ui::file_name(today));
        std::fs::create_dir_all(&dir)
            .and_then(|()| std::fs::write(&path, atropos_core::export::to_json(&doc)))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))?;
        Ok(path)
    }

    fn open_affinity(&mut self, key: ProcKey) {
        let Some(f) = &self.frame else {
            return;
        };
        let name = f
            .snapshot
            .get(key)
            .map_or_else(|| "process".to_owned(), |p| p.raw.image.to_string());
        let mut p = WindowsPlatform::new();
        let current =
            atropos_core::platform::Platform::pin(&mut p, key, atropos_core::pin::Access::QUERY)
                .ok()
                .and_then(|pin| atropos_core::platform::Platform::tuning(&mut p, &pin).ok())
                .map(|t| t.affinity);
        self.affinity = Some(crate::affinity::AffinityUi::new(
            key,
            name,
            f.snapshot.stats.logical_cpus,
            current,
        ));
    }

    fn on_affinity(&mut self, ctx: &egui::Context) {
        let Some(st) = &mut self.affinity else {
            return;
        };
        match crate::affinity::show(ctx, st) {
            Some(crate::affinity::AffinityEvent::Apply(key, mask)) => {
                match self.tune(key, atropos_core::tune::Tune::Affinity(mask)) {
                    Ok(()) => self.affinity = None,
                    Err(e) => {
                        if let Some(st) = &mut self.affinity {
                            st.fail(e);
                        }
                    }
                }
            }
            Some(crate::affinity::AffinityEvent::Close) => self.affinity = None,
            None => {}
        }
    }

    /// `Ctrl+P`: resume if it is suspended, else suspend.
    fn toggle_suspend(&mut self, key: ProcKey) {
        let suspended = self
            .frame
            .as_ref()
            .and_then(|f| f.snapshot.get(key))
            .is_some_and(|p| p.raw.suspended);
        self.act(key, if suspended { Act::Resume } else { Act::Suspend });
    }

    fn top_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let r = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text(SEARCH_HINT)
                    .desired_width(380.0),
            );
            if std::mem::take(&mut self.keys.focus_search) {
                r.request_focus();
            }
            if r.changed() {
                self.filter = Filter::parse(&self.search);
                self.changed_view();
            }
            self.scope_buttons(ui);
            ui.separator();
            if ui
                .add_enabled(
                    self.opts.scope == Scope::All,
                    egui::Checkbox::new(&mut self.opts.tree, "Tree"),
                )
                .changed()
            {
                self.changed_view();
            }
            if ui
                .checkbox(&mut self.opts.per_core, "Per-core CPU")
                .on_hover_text("100 % = one full core (htop style) instead of the whole machine")
                .changed()
            {
                self.dirty = true;
            }
            if ui
                .add_enabled(
                    self.opts.scope != Scope::Blast,
                    egui::Checkbox::new(&mut self.opts.gpu, "GPU"),
                )
                .on_hover_text("Show GPU use and GPU memory per process (read only while shown)")
                .changed()
            {
                self.dirty = true;
            }
            if self.freeze.full {
                ui.label("⏸ Order frozen (Space)");
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui
                    .small_button("?")
                    .on_hover_text("Keyboard shortcuts")
                    .clicked()
                {
                    self.keys.help = !self.keys.help;
                }
                if ui.small_button("⚙").on_hover_text("Settings").clicked() {
                    self.settings.open = !self.settings.open;
                    if self.settings.open {
                        self.settings.explorer_menu = Some(explorer_menu::is_installed());
                    }
                }
                if ui
                    .small_button("Export…")
                    .on_hover_text("Save every process as JSON, for a bug report")
                    .clicked()
                {
                    self.export.open = !self.export.open;
                    self.export.result = None;
                }
                if ui
                    .small_button("History")
                    .on_hover_text("What Atropos ended, with Restart")
                    .clicked()
                {
                    self.history.open = !self.history.open;
                    if self.history.open {
                        self.history.entries =
                            atropos_core::history::read(&atropos_core::history::path());
                    }
                }
                if ui
                    .small_button("What is locking…")
                    .on_hover_text(
                        "Find what has a file or folder open (also: lock <path> in the palette)",
                    )
                    .clicked()
                {
                    self.palette.summon_with("lock ");
                }
                if let Some(f) = &self.frame {
                    ui.label(format!(
                        "{} of {} processes",
                        self.rows.len(),
                        f.snapshot.procs.len()
                    ));
                }
            });
        });
    }

    /// One button per scope (All, Projects, Leftovers, Blast Radius, Disk).
    fn scope_buttons(&mut self, ui: &mut egui::Ui) {
        for (scope, text, tip) in [
            (Scope::All, "All", "Every process"),
            (
                Scope::Projects,
                "Projects",
                "Processes running inside a git repository, grouped by repository",
            ),
            (
                Scope::Leftovers,
                "Leftovers",
                "Dev servers left behind: listening on a port, inside a repository, \
             and whatever started them is gone",
            ),
            (
                Scope::Blast,
                "Blast Radius",
                "The process tree as a map: each block is sized by the memory of the \
             process and everything it started, and coloured by CPU",
            ),
            (
                Scope::Disk,
                "Disk",
                "Processes reading or writing the disk right now, busiest first",
            ),
        ] {
            if ui
                .selectable_label(self.opts.scope == scope, text)
                .on_hover_text(tip)
                .clicked()
                && self.opts.scope != scope
            {
                self.opts.scope = scope;
                self.changed_view();
            }
        }
    }

    /// Header, search bar and (in the Projects scope) the repository strip.
    fn top_panel(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            if let Some(f) = self.frame.clone() {
                header::show(ui, &f);
                ui.add_space(2.0);
            }
            self.top_bar(ui);
            if let (Scope::Projects, Some(f)) = (self.opts.scope, self.frame.clone()) {
                let groups = atropos_core::projects::groups(&f.snapshot, &f.context);
                match projects_bar::show(ui, &groups, self.opts.project.as_ref()) {
                    Some(BarEvent::Pick(repo)) => {
                        self.opts.project = repo;
                        self.changed_view();
                    }
                    Some(BarEvent::KillAll(repo)) => self.actions.start_group(&repo, &f),
                    None => {}
                }
            }
            ui.add_space(2.0);
        });
    }

    /// Applies this frame's keyboard shortcuts; returns whether `F5` asked for a refresh.
    fn on_keys(
        &mut self,
        ctx: &egui::Context,
        today: Option<atropos_core::platform::windows::LocalTime>,
    ) -> bool {
        if self.palette.open {
            return false; // the palette takes every key while it is open
        }
        let typing = ctx.wants_keyboard_input();
        let mut refresh = false;
        for a in ctx.input(|i| keys::actions(&i.events, typing)) {
            match a {
                Action::FocusSearch => self.keys.focus_search = true,
                Action::OpenDetails => self.details.open = self.selection.0.is_some(),
                Action::CopyRow => {
                    let row = self
                        .selection
                        .0
                        .and_then(|k| self.rows.iter().find(|r| r.key == k));
                    if let (Some(row), Some(f)) = (row, &self.frame) {
                        ctx.copy_text(table::row_text(f, row, self.opts.per_core, today));
                    }
                }
                Action::ToggleTree => {
                    self.opts.tree = !self.opts.tree;
                    self.changed_view();
                }
                Action::Refresh => refresh = true,
                Action::ToggleHelp => self.keys.help = !self.keys.help,
                Action::ToggleFreeze => {
                    self.freeze.full = !self.freeze.full;
                    if !self.freeze.full {
                        self.freeze.invalidate();
                    }
                    self.dirty = true;
                }
                Action::Kill | Action::KillTree | Action::ToggleSuspend | Action::Escape
                    if self.actions.holds() => {}
                Action::Kill => {
                    if let Some(k) = self.selection.0 {
                        self.act(k, Act::Kill);
                    }
                }
                Action::KillTree => {
                    if let Some(k) = self.selection.0 {
                        self.act(k, Act::KillTree);
                    }
                }
                Action::ToggleSuspend => {
                    if let Some(k) = self.selection.0 {
                        self.toggle_suspend(k);
                    }
                }
                Action::Escape => {
                    if self.keys.help {
                        self.keys.help = false;
                    } else if !typing {
                        self.details.open = false;
                    }
                }
            }
        }
        refresh
    }

    /// The table, or the treemap (Blast Radius) or the ranked list (Disk) in those scopes.
    fn central(
        &mut self,
        ui: &mut egui::Ui,
        today: Option<atropos_core::platform::windows::LocalTime>,
    ) {
        let Some(frame) = self.frame.clone() else {
            ui.centered_and_justified(|ui| ui.label("Reading processes…"));
            return;
        };
        let look = Look {
            selected: self.selection.0,
            sort: self.opts.sort,
            per_core: self.opts.per_core,
            today,
            gpu: self.opts.gpu,
        };
        if self.rows.is_empty() && self.opts.scope == Scope::Leftovers && self.filter.is_empty() {
            ui.label(
                "No leftovers: nothing is listening on a port from inside a repository \
                 without the process that started it.",
            );
        }
        if self.opts.scope == Scope::Blast {
            self.pointer_on_table = false;
            for e in blast::show(ui, &frame, &mut self.blast, self.selection.0) {
                match e {
                    BlastEvent::Select(k) => self.selection.0 = Some(k),
                    BlastEvent::Zoom(z) => self.blast.zoom = z,
                }
            }
            return;
        }
        if self.opts.scope == Scope::Disk {
            self.pointer_on_table = false;
            if let Some(k) = crate::disk::show(ui, &frame, &self.rows, self.selection.0) {
                self.selection.0 = Some(k);
            }
            return;
        }
        let events = table::show(ui, &frame, &self.rows, look, &self.actions);
        self.pointer_on_table = ui.ui_contains_pointer();
        for e in events {
            self.on_event(e);
        }
    }

    /// Hotkey and tray requests: bring the window back (and open the palette), or quit.
    fn on_summons(&mut self, ctx: &egui::Context) {
        let Some(summoner) = &self.summoner else {
            return;
        };
        for s in summoner.poll() {
            if s == Summon::Quit {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                continue;
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
            ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            if s == Summon::Palette {
                self.palette.summon();
            }
        }
    }

    fn on_palette(&mut self, ctx: &egui::Context) {
        let Some(f) = self.frame.clone() else {
            return;
        };
        let me = self.me.clone().unwrap_or_default();
        match palette::show(ctx, &mut self.palette, &f, &me, &mut self.matcher) {
            Some(PaletteEvent::Kill(k)) => self.act(k, Act::Kill),
            Some(PaletteEvent::Show(k)) => {
                self.selection.0 = Some(k);
                self.details.open = true;
            }
            Some(PaletteEvent::Lookup(path)) => self.lookup_lock(ctx, path),
            None => {}
        }
    }

    /// `--lock <path>` (the Explorer menu): open the palette on that path and look it up.
    fn with_lock_request(mut self, ctx: &egui::Context) -> Self {
        if let Some(path) = self.args.lock.clone() {
            self.palette
                .summon_with(&format!("lock \"{}\"", path.display()));
            self.lookup_lock(ctx, path);
        }
        self
    }

    /// Starts "what has `path` open?" on its own thread (it may take up to
    /// `locks::TIMEOUT`); the answer lands in the palette.
    fn lookup_lock(&mut self, ctx: &egui::Context, path: std::path::PathBuf) {
        let (tx, rx) = crossbeam_channel::bounded(1);
        let ctx = ctx.clone();
        self.palette.looking = Some(path.clone());
        let spawned = std::thread::Builder::new()
            .name("atropos-lock".into())
            .spawn(move || {
                let r = atropos_core::platform::Platform::file_lockers(
                    &mut WindowsPlatform::new(),
                    &path,
                )
                .map_err(|e| lock_message(&e));
                let _ = tx.send((path, r));
                ctx.request_repaint();
            });
        match spawned {
            Ok(_) => self.lock_rx = Some(rx),
            Err(e) => {
                let path = self.palette.looking.take().unwrap_or_default();
                self.palette.lock = Some((path, Err(format!("Could not start the lookup: {e}"))));
            }
        }
    }

    fn poll_lock(&mut self) {
        let Some(rx) = &self.lock_rx else {
            return;
        };
        if let Ok(answer) = rx.try_recv() {
            self.palette.looking = None;
            self.palette.lock = Some(answer);
            self.lock_rx = None;
        }
    }

    /// The History window: reload after a new entry, and Restart what the user picked.
    fn on_history(
        &mut self,
        ctx: &egui::Context,
        today: Option<atropos_core::platform::windows::LocalTime>,
        now: Instant,
    ) {
        if self.actions.take_history_changed() && self.history.open {
            self.history.entries = atropos_core::history::read(&atropos_core::history::path());
        }
        if !self.history.open {
            return;
        }
        let Some(launch) = crate::history_view::show(ctx, &mut self.history, today) else {
            return;
        };
        let elevated = atropos_core::platform::windows::current_process_elevated();
        match atropos_core::history::restart(&mut WindowsPlatform::new(), &launch, elevated) {
            Ok(k) => self
                .actions
                .info(format!("Started again as PID {}", k.pid), now),
            Err(e) => self.actions.notice(e, now),
        }
    }

    /// Installs or removes the Explorer entry the user just (un)ticked; a failed install leaves
    /// nothing half written.
    fn set_explorer_menu(&mut self, install: bool, now: Instant) {
        let result = if install {
            std::env::current_exe()
                .map_err(|e| e.to_string())
                .and_then(|exe| {
                    explorer_menu::install(&exe).map_err(|e| {
                        let _ = explorer_menu::remove();
                        e.to_string()
                    })
                })
        } else {
            explorer_menu::remove().map_err(|e| e.to_string())
        };
        if let Err(e) = result {
            tracing::warn!("explorer menu: {e}");
            self.actions
                .notice(format!("Could not change the Explorer menu: {e}"), now);
        }
        self.settings.explorer_menu = Some(explorer_menu::is_installed());
    }

    /// Applies and saves changed settings (the file is created only now, Z3).
    fn apply_settings(&mut self, ctx: &egui::Context, now: Instant) {
        if self.config.hotkey != self.bound_hotkey {
            self.bound_hotkey.clone_from(&self.config.hotkey);
            if let Some(n) = self
                .summoner
                .as_mut()
                .and_then(|s| s.rebind(&self.config.hotkey))
            {
                self.actions.notice(n, now);
            }
        }
        ctx.set_theme(settings::theme(self.config.theme));
        self.actions.grace = std::time::Duration::from_secs(u64::from(self.config.grace_secs));
        self.actions.confirm = self.config.confirm;
        if let Err(e) = config::save(&config::path(), &self.config) {
            self.actions
                .notice(format!("Could not save settings: {e}"), now);
        }
    }

    fn on_first_frame(&mut self, ctx: &egui::Context) {
        self.first_frame_done = true;
        let ms = startup::elapsed_ms(self.create_time, now_filetime());
        tracing::debug!(first_frame_ms = ms, "first frame");
        self.append_log(&format!("first_frame_ms={ms:.1}"));
        if self.args.exit_after_first_frame {
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }

    fn log_heap(&self) {
        self.append_log(&startup::heap_line(crate::ALLOC.live_bytes()));
    }

    /// Appends one line to `--startup-log`, if given (perf.ps1 reads it).
    fn append_log(&self, line: &str) {
        let Some(path) = &self.args.startup_log else {
            return;
        };
        let res = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .and_then(|mut f| writeln!(f, "{line}"));
        if let Err(e) = res {
            tracing::warn!("startup log {}: {e}", path.display());
        }
    }
}

impl eframe::App for AtroposApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let frame_start = Instant::now();
        self.poll();
        let now = Instant::now();
        self.actions.poll(now);
        let today = local_time(now_filetime());
        self.on_summons(ctx);
        self.poll_lock();
        let refresh = self.on_keys(ctx, today);
        self.steer(ctx, refresh);

        self.top_panel(ctx);
        self.recompute();
        if let (Some(key), Some(f), true) =
            (self.selection.0, self.frame.clone(), self.details.open)
        {
            let mut close = false;
            egui::SidePanel::right("details")
                .resizable(true)
                .default_width(380.0)
                .show(ctx, |ui| {
                    let ev = details::show(
                        ui,
                        &f,
                        key,
                        &mut self.details,
                        today,
                        &self.config.mask_names,
                    );
                    close |= ev == Some(DetailEvent::Close);
                });
            if close {
                self.details.open = false;
            }
        }
        egui::Window::new("Keyboard shortcuts")
            .open(&mut self.keys.help)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                egui::Grid::new("help").num_columns(2).show(ui, |ui| {
                    for (k, what) in keys::HELP {
                        ui.label(egui::RichText::new(k).monospace().strong());
                        ui.label(what);
                        ui.end_row();
                    }
                });
            });
        egui::CentralPanel::default().show(ctx, |ui| self.central(ui, today));
        self.on_palette(ctx);
        if let Some(f) = self.frame.clone() {
            self.actions.show(ctx, &f, today, now);
        }
        if let Some(offer) = &self.resume_offer {
            if let Some(choice) = crate::crash::show(ctx, offer) {
                let items: Vec<(ProcKey, u32)> =
                    offer.items.iter().map(|(k, n, _)| (*k, *n)).collect();
                self.actions
                    .settle_leftovers(&items, choice == crate::crash::Choice::ResumeAll);
                self.resume_offer = None;
            }
        }
        if self.settings.open && settings::show(ctx, &mut self.settings, &mut self.config) {
            self.apply_settings(ctx, now);
        }
        if let Some(want) = self.settings.explorer_request.take() {
            self.set_explorer_menu(want, now);
        }
        self.on_history(ctx, today, now);
        self.on_affinity(ctx);
        if let Some(unmasked) = crate::export_ui::show(ctx, &mut self.export) {
            self.export.result = Some(self.export_snapshot(unmasked, today));
        }
        if self.dirty {
            // A view change made this frame: show it now rather than at the next tick.
            ctx.request_repaint();
        }
        if !self.first_frame_done {
            self.on_first_frame(ctx);
        }
        if let Some(log) = &mut self.perf_log {
            let _ = writeln!(
                log,
                "frame_ms={:.3}",
                frame_start.elapsed().as_secs_f64() * 1000.0
            );
        }
    }

    #[cfg(feature = "renderer-glow")]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.log_heap();
    }

    #[cfg(feature = "renderer-wgpu")]
    fn on_exit(&mut self) {
        self.log_heap();
    }
}

/// The palette's words for a failed lookup.
fn lock_message(e: &atropos_core::CoreError) -> String {
    use atropos_core::CoreError;
    match e {
        CoreError::NotFound => "That file or folder does not exist.".into(),
        CoreError::TimedOut => format!(
            "No answer after {} s (an unreachable network share?).",
            atropos_core::locks::TIMEOUT.as_secs()
        ),
        other => format!("Windows could not say what has it open ({other})."),
    }
}
