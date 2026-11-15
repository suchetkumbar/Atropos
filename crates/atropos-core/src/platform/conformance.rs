//! The conformance suite (spec Phase 7, roadmap M7.1): one set of scenarios that every
//! [`Platform`] must pass. The fake backend runs it in `atropos-core`'s own tests, and each
//! real backend runs it in the `fixtures` crate against processes the test spawned itself.
//!
//! A scenario acts only on keys its [`Backend`] returned from a `spawn_*` call (the safety
//! rule: never a process the test did not start). It returns `Err(why)` instead of panicking,
//! so the runner names the scenario and the reason.

use std::path::Path;
use std::time::{Duration, Instant};

use super::{Platform, RawProc};
use crate::engine::kill::{KillOpts, Outcome, kill};
use crate::engine::subtree::{kill_tree, preorder};
use crate::pin::{Access, PinError};
use crate::snapshot::{Snapshot, build};
use crate::tune::{ACCESS as TUNE, Priority, Tune};
use crate::{Field, ProcKey};

/// A backend under test, plus a way to start processes the test owns. Every `spawn_*` fails
/// with a readable reason (the fixture could not start or did not report in).
#[allow(clippy::missing_errors_doc)] // the trait doc covers every method's error
pub trait Backend {
    fn platform(&mut self) -> &mut dyn Platform;
    /// A long-running process with these extra arguments (e.g. the `sleeper` fixture).
    fn spawn(&mut self, args: &[&str]) -> Result<ProcKey, String>;
    /// A process that starts at least one child of its own; returns the parent.
    fn spawn_tree(&mut self) -> Result<ProcKey, String>;
    /// A process listening on a loopback TCP port; returns it and the port.
    fn spawn_listener(&mut self) -> Result<(ProcKey, u16), String>;
    /// A process that keeps `path` open.
    fn spawn_locker(&mut self, path: &Path) -> Result<ProcKey, String>;
    /// How long a scenario waits for the OS to catch up.
    fn patience(&self) -> Duration {
        Duration::from_secs(5)
    }
}

type Scenario = fn(&mut dyn Backend) -> Result<(), String>;

/// Every scenario, by name.
pub const ALL: [(&str, Scenario); 8] = [
    ("scan lists a spawned process", scan_lists_a_spawned_process),
    (
        "statics carry the command line",
        statics_carry_the_command_line,
    ),
    (
        "kill ends it and a later pin is Gone",
        kill_ends_it_and_a_later_pin_is_gone,
    ),
    (
        "suspend and resume round-trip",
        suspend_and_resume_round_trip,
    ),
    (
        "subtree kill ends every member",
        subtree_kill_ends_every_member,
    ),
    (
        "a listener is found by its port",
        a_listener_is_found_by_its_port,
    ),
    ("a locker is found by key", a_locker_is_found_by_key),
    ("tuning round-trips", tuning_round_trips),
];

/// Runs every scenario; returns the failures as `name: reason`.
#[must_use]
pub fn run_all(b: &mut dyn Backend) -> Vec<String> {
    ALL.iter()
        .filter_map(|(name, f)| f(b).err().map(|e| format!("{name}: {e}")))
        .collect()
}

fn snapshot(p: &mut dyn Platform) -> Result<Snapshot, String> {
    let mut raw: Vec<RawProc> = Vec::new();
    let stats = p.scan(&mut raw).map_err(|e| format!("scan failed: {e}"))?;
    Ok(build(None, raw, stats))
}

/// Polls `cond` for up to `wait` (real backends need a moment; the fake answers at once).
fn eventually(
    p: &mut dyn Platform,
    wait: Duration,
    what: &str,
    mut cond: impl FnMut(&mut dyn Platform) -> Result<bool, String>,
) -> Result<(), String> {
    let start = Instant::now();
    loop {
        if cond(p)? {
            return Ok(());
        }
        if start.elapsed() > wait {
            return Err(format!("timed out waiting until {what}"));
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

const FORCE: KillOpts = KillOpts {
    grace: Duration::from_millis(200),
    force: true,
};

fn scan_lists_a_spawned_process(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let key = b.spawn(&[])?;
    eventually(b.platform(), wait, "the process is in a scan", |p| {
        Ok(snapshot(p)?.get(key).is_some())
    })
}

fn statics_carry_the_command_line(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let key = b.spawn(&["conformance-marker"])?;
    eventually(b.platform(), wait, "its command line is readable", |p| {
        Ok(match p.statics(key).cmdline {
            Field::Ok(c) => c.contains("conformance-marker"),
            _ => false,
        })
    })
}

fn kill_ends_it_and_a_later_pin_is_gone(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let key = b.spawn(&[])?;
    let p = b.platform();
    match kill(p, key, FORCE, false, &mut |_| {}) {
        Outcome::Terminated | Outcome::Exited => {}
        o => return Err(format!("kill returned {o:?}")),
    }
    eventually(p, wait, "it is gone from the scan", |p| {
        Ok(snapshot(p)?.get(key).is_none())
    })?;
    match p.pin(key, Access::QUERY) {
        Err(PinError::Gone | PinError::Reused) => Ok(()),
        other => Err(format!("pin after the kill gave {other:?}")),
    }
}

fn suspend_and_resume_round_trip(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let key = b.spawn(&[])?;
    let p = b.platform();
    let pin = p
        .pin(key, Access::SUSPEND)
        .map_err(|e| format!("pin: {e}"))?;
    p.suspend(&pin).map_err(|e| format!("suspend: {e}"))?;
    eventually(p, wait, "the scan shows it suspended", |p| {
        Ok(snapshot(p)?.get(key).is_some_and(|x| x.raw.suspended))
    })?;
    p.resume(&pin).map_err(|e| format!("resume: {e}"))?;
    eventually(p, wait, "the scan shows it running", |p| {
        Ok(snapshot(p)?.get(key).is_some_and(|x| !x.raw.suspended))
    })
}

fn subtree_kill_ends_every_member(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let root = b.spawn_tree()?;
    let p = b.platform();
    let mut members = Vec::new();
    eventually(p, wait, "the parent has a child", |p| {
        let snap = snapshot(p)?;
        members = snap
            .index
            .get(&root)
            .map(|&i| preorder(&snap, i))
            .unwrap_or_default();
        Ok(members.len() >= 2)
    })?;
    let report = kill_tree(p, root, FORCE, &|_| false, &mut |_| {});
    if let Some((k, o)) = report
        .members
        .iter()
        .find(|(_, o)| !matches!(o, Outcome::Terminated | Outcome::Exited | Outcome::Gone))
    {
        return Err(format!("member {k:?} ended as {o:?}"));
    }
    eventually(p, wait, "no member is left", |p| {
        let snap = snapshot(p)?;
        Ok(members.iter().all(|k| snap.get(*k).is_none()))
    })
}

fn a_listener_is_found_by_its_port(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let (key, port) = b.spawn_listener()?;
    eventually(b.platform(), wait, "its socket is listed", |p| {
        let snap = snapshot(p)?;
        let sockets = p.sockets().map_err(|e| format!("sockets: {e}"))?;
        Ok(crate::ports::owners(&snap, &sockets, port)
            .iter()
            .any(|(k, _)| *k == key))
    })
}

fn a_locker_is_found_by_key(b: &mut dyn Backend) -> Result<(), String> {
    let wait = b.patience();
    let dir = std::env::temp_dir().join(format!("atropos conformance lock {}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let file = dir.join("held.txt");
    let key = b.spawn_locker(&file)?;
    let r = eventually(b.platform(), wait, "the locker is reported", |p| {
        Ok(p.file_lockers(&file)
            .map_err(|e| format!("file_lockers: {e}"))?
            .contains(&key))
    });
    let _ = std::fs::remove_dir_all(&dir);
    r
}

fn tuning_round_trips(b: &mut dyn Backend) -> Result<(), String> {
    let key = b.spawn(&[])?;
    let p = b.platform();
    let pin = p.pin(key, TUNE).map_err(|e| format!("pin: {e}"))?;
    p.tune(&pin, Tune::Priority(Priority::BelowNormal))
        .map_err(|e| format!("set priority: {e}"))?;
    let t = p.tuning(&pin).map_err(|e| format!("read back: {e}"))?;
    if t.priority != Priority::BelowNormal {
        return Err(format!("priority reads back as {:?}", t.priority));
    }
    Ok(())
}

/// The fake backend as a [`Backend`]: "spawning" adds a scripted process.
#[derive(Debug, Default)]
pub struct FakeBackend {
    pub fake: super::FakePlatform,
    next_pid: u32,
}

impl FakeBackend {
    #[must_use]
    pub fn new() -> Self {
        Self {
            fake: super::FakePlatform::new(4),
            next_pid: 100,
        }
    }

    fn start(&mut self, ppid: u32, args: &[&str]) -> ProcKey {
        self.next_pid += 1;
        let key = ProcKey {
            pid: self.next_pid,
            create_time: u64::from(self.next_pid) * 7,
        };
        self.fake.start(RawProc::new(key, ppid, "sleeper.exe"));
        self.fake.set_statics(
            key,
            crate::statics::Statics {
                cmdline: Field::Ok(std::sync::Arc::from(
                    format!("sleeper.exe {}", args.join(" ")).as_str(),
                )),
                ..crate::statics::Statics::unavailable(&Field::Unsupported)
            },
        );
        key
    }
}

impl Backend for FakeBackend {
    fn platform(&mut self) -> &mut dyn Platform {
        &mut self.fake
    }

    fn spawn(&mut self, args: &[&str]) -> Result<ProcKey, String> {
        Ok(self.start(1, args))
    }

    fn spawn_tree(&mut self) -> Result<ProcKey, String> {
        let root = self.start(1, &[]);
        self.start(root.pid, &[]);
        Ok(root)
    }

    fn spawn_listener(&mut self) -> Result<(ProcKey, u16), String> {
        let key = self.start(1, &[]);
        let port = 40_000 + u16::try_from(key.pid % 1000).unwrap_or(0);
        self.fake.sockets.push(crate::ports::Socket {
            proto: crate::ports::Proto::Tcp,
            local: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            remote: None,
            state: Some(crate::ports::TcpState::Listen),
            owner: key.pid,
        });
        Ok((key, port))
    }

    fn spawn_locker(&mut self, path: &Path) -> Result<ProcKey, String> {
        let key = self.start(1, &[]);
        self.fake.lockers.push((path.to_path_buf(), vec![key]));
        Ok(key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_fake_backend_passes_the_conformance_suite() {
        let failures = run_all(&mut FakeBackend::new());
        assert_eq!(failures, Vec::<String>::new());
    }

    #[test]
    fn a_broken_backend_fails_with_the_scenario_name() {
        /// Scans never list anything.
        struct Blind(FakeBackend);
        impl Backend for Blind {
            fn platform(&mut self) -> &mut dyn Platform {
                self.0.fake.exit_all();
                &mut self.0.fake
            }
            fn spawn(&mut self, a: &[&str]) -> Result<ProcKey, String> {
                self.0.spawn(a)
            }
            fn spawn_tree(&mut self) -> Result<ProcKey, String> {
                self.0.spawn_tree()
            }
            fn spawn_listener(&mut self) -> Result<(ProcKey, u16), String> {
                self.0.spawn_listener()
            }
            fn spawn_locker(&mut self, p: &Path) -> Result<ProcKey, String> {
                self.0.spawn_locker(p)
            }
            fn patience(&self) -> Duration {
                Duration::from_millis(30)
            }
        }
        let failures = run_all(&mut Blind(FakeBackend::new()));
        assert!(
            failures
                .iter()
                .any(|f| f.starts_with("scan lists a spawned process:")),
            "{failures:?}"
        );
    }
}
