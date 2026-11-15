//! `atropos-tui`: the terminal front end over `atropos-core` (spec Phase 7), for SSH sessions
//! and terminals without a desktop. Same filter syntax, same policy, same kill engine as the
//! GUI and `atr`.

mod model;
mod ui;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use atropos_core::ProcKey;
use atropos_core::collector::{self, Frame, Interest, Visibility};
use atropos_core::engine::kill::{KillOpts, Outcome, kill};
use atropos_core::engine::policy::{self, ActionKind, Verdict};
use atropos_core::engine::subtree::{kill_tree, preorder};
use atropos_core::platform::Native;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use crate::model::{Command, Model};

fn outcome_text(o: Outcome) -> &'static str {
    match o {
        Outcome::Exited => "closed",
        Outcome::Terminated => "ended (forced)",
        Outcome::Denied => "refused by the OS (another user's process?)",
        Outcome::Protected => "protected",
        Outcome::Gone => "already gone",
        Outcome::Stuck => "still running after being forced",
    }
}

/// Subtree members the policy hard-blocks (they are skipped by a tree kill).
fn hard_blocked(frame: &Frame, me: ProcKey, root: ProcKey) -> HashSet<ProcKey> {
    let ctx = policy::Context {
        snapshot: &frame.snapshot,
        statics: &frame.statics,
        services: &[],
        me,
    };
    frame
        .snapshot
        .index
        .get(&root)
        .map(|&i| preorder(&frame.snapshot, i))
        .unwrap_or_default()
        .into_iter()
        .filter(|k| matches!(policy::check(&ctx, *k, ActionKind::Kill), Verdict::Hard(_)))
        .collect()
}

/// Ends `key` (and its subtree) on its own thread; the result comes back as a message.
fn start_kill(
    frame: Option<Arc<Frame>>,
    me: ProcKey,
    key: ProcKey,
    tree: bool,
    done: crossbeam_channel::Sender<String>,
) {
    let name = frame
        .as_ref()
        .and_then(|f| f.snapshot.get(key))
        .map_or_else(|| "process".to_owned(), |p| p.raw.image.to_string());
    let blocked = frame.map_or_else(HashSet::new, |f| hard_blocked(&f, me, key));
    std::thread::spawn(move || {
        let mut p = Native::new();
        let opts = KillOpts {
            grace: Duration::from_secs(5),
            force: false,
        };
        let text = if tree {
            let r = kill_tree(&mut p, key, opts, &|k| blocked.contains(&k), &mut |_| {});
            let ended = r
                .members
                .iter()
                .filter(|(_, o)| matches!(o, Outcome::Exited | Outcome::Terminated))
                .count();
            format!(
                "{name} ({}): {ended} of {} processes ended",
                key.pid,
                r.members.len()
            )
        } else {
            let o = kill(&mut p, key, opts, false, &mut |_| {});
            format!("{name} ({}): {}", key.pid, outcome_text(o))
        };
        let _ = done.send(text);
    });
}

fn main() -> anyhow::Result<()> {
    let me = atropos_core::platform::current_process_key()
        .map_err(|e| anyhow::anyhow!("cannot read own process identity: {e}"))?;
    let collector = collector::spawn_with(Box::new(Native::new()), Box::new(|| {}), Vec::new())?;
    let _ = collector.interest.send(Interest {
        visibility: Visibility::Focused,
        ..Interest::default()
    });
    let (done_tx, done_rx) = crossbeam_channel::unbounded();
    let mut model = Model::new(me);
    let mut terminal = ratatui::init();
    let result = (|| -> anyhow::Result<()> {
        loop {
            if let Some(f) = collector.frames.try_iter().last() {
                model.set_frame(f);
            }
            if let Ok(text) = done_rx.try_recv() {
                model.message = Some(text);
            }
            terminal.draw(|f| ui::draw(f, &model))?;
            if !event::poll(Duration::from_millis(250))? {
                continue;
            }
            let Event::Key(k) = event::read()? else {
                continue;
            };
            if k.kind != KeyEventKind::Press {
                continue;
            }
            match model.on_key(k) {
                Command::Quit => return Ok(()),
                Command::Kill { key, tree } => {
                    model.message = Some(format!("Ending {}…", key.pid));
                    start_kill(model.frame.clone(), me, key, tree, done_tx.clone());
                }
                Command::None => {}
            }
        }
    })();
    ratatui::restore();
    result
}
