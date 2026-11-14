//! The main window: search bar and toggles on top, the process table below. Repaints only when
//! the collector delivers a frame or the user interacts (no `request_repaint` loop).

use std::io::Write as _;
use std::sync::Arc;
use std::time::Instant;

use atropos_core::ProcKey;
use atropos_core::collector::{self, Collector, Frame, Interest, Visibility};
use atropos_core::filter::{Filter, FilterMatcher};
use atropos_core::platform::windows::{
    WindowsPlatform, current_process_key, local_time, now_filetime,
};
use eframe::egui;

use crate::actions::{Act, Actions};
use crate::args::Args;
use crate::details::{self, DetailEvent, DetailsState};
use crate::header;
use crate::keys::{self, Action};
use crate::selection::{OrderFreeze, Selection};
use crate::startup;
use crate::table::{self, Event, Look};
use crate::view::{self, Row, Sort, SortKey, ViewOpts};

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
}

impl AtroposApp {
    pub fn new(cc: &eframe::CreationContext<'_>, args: Args, create_time: u64) -> Self {
        let ctx = cc.egui_ctx.clone();
        let collector = collector::spawn(
            Box::new(WindowsPlatform::new()),
            // A non-zero delay paints once; `request_repaint()` always paints twice.
            Box::new(move || ctx.request_repaint_after(std::time::Duration::from_millis(1))),
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
        let actions = Actions::spawn(&cc.egui_ctx, self_key);
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
            opts: ViewOpts::default(),
            selection: Selection::default(),
            details: DetailsState::default(),
            keys: KeyState::default(),
            freeze: OrderFreeze::default(),
            rows: Vec::new(),
            dirty: false,
            pointer_on_table: false,
            perf_log,
            actions,
        }
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
            want_ports: self.filter.has_port_term()
                || (self.details.open && self.details.tab == details::Tab::Ports),
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
        }
    }

    fn act(&mut self, key: ProcKey, act: Act) {
        if let Some(f) = &self.frame {
            self.actions.start(key, act, f);
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
            if ui.checkbox(&mut self.opts.tree, "Tree").changed() {
                self.changed_view();
            }
            if ui
                .checkbox(&mut self.opts.per_core, "Per-core CPU")
                .on_hover_text("100 % = one full core (htop style) instead of the whole machine")
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

    /// Applies this frame's keyboard shortcuts; returns whether `F5` asked for a refresh.
    fn on_keys(
        &mut self,
        ctx: &egui::Context,
        today: Option<atropos_core::platform::windows::LocalTime>,
    ) -> bool {
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
        let refresh = self.on_keys(ctx, today);
        self.steer(ctx, refresh);

        egui::TopBottomPanel::top("bar").show(ctx, |ui| {
            ui.add_space(4.0);
            if let Some(f) = self.frame.clone() {
                header::show(ui, &f);
                ui.add_space(2.0);
            }
            self.top_bar(ui);
            ui.add_space(2.0);
        });
        self.recompute();
        if let (Some(key), Some(f), true) =
            (self.selection.0, self.frame.clone(), self.details.open)
        {
            let mut close = false;
            egui::SidePanel::right("details")
                .resizable(true)
                .default_width(380.0)
                .show(ctx, |ui| {
                    let ev = details::show(ui, &f, key, &mut self.details, today);
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
        egui::CentralPanel::default().show(ctx, |ui| {
            let Some(frame) = self.frame.clone() else {
                ui.centered_and_justified(|ui| ui.label("Reading processes…"));
                return;
            };
            let look = Look {
                selected: self.selection.0,
                sort: self.opts.sort,
                per_core: self.opts.per_core,
                today,
            };
            let events = table::show(ui, &frame, &self.rows, look, &self.actions);
            self.pointer_on_table = ui.ui_contains_pointer();
            for e in events {
                self.on_event(e);
            }
        });
        if let Some(f) = self.frame.clone() {
            self.actions.show(ctx, &f, today, now);
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
