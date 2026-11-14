//! Stopping one process (spec Phase 2): pin → graceful strategy → grace period → force →
//! `Stuck` if still alive 2 s after `TerminateProcess`. Exactly one [`Outcome`] per target.

use std::time::Duration;

use crate::ProcKey;
use crate::pin::{Access, PinError};
use crate::platform::Platform;

/// The honest result of an action (spec: exactly one of these).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Exited on its own after the graceful request.
    Exited,
    /// Forced to stop.
    Terminated,
    /// The OS refused (e.g. a standard user and an elevated target).
    Denied,
    /// Refused by policy (hard-blocked / protected).
    Protected,
    /// Already gone (or the PID was reused by another process).
    Gone,
    /// Still alive 2 s after `TerminateProcess`.
    Stuck,
}

/// How to ask a process to stop before forcing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Strategy {
    /// Stop the services this process hosts (`ControlService(SERVICE_CONTROL_STOP)`).
    Service(Vec<String>),
    /// `WM_CLOSE` to its visible, unowned top-level windows.
    Window,
    /// Ctrl+C through the console helper (`atr.exe __ctrl-c`).
    ConsoleCtrlC,
    /// Nothing graceful applies: force right away.
    None,
}

/// What the user asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KillOpts {
    /// How long the graceful request may take before forcing (spec default 5 s).
    pub grace: Duration,
    /// Skip the graceful step.
    pub force: bool,
}

impl Default for KillOpts {
    fn default() -> Self {
        Self {
            grace: Duration::from_secs(5),
            force: false,
        }
    }
}

/// Live progress for the GUI row (`Closing… 3s`) and the CLI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    Closing { strategy: Strategy, left: Duration },
    Forcing,
}

/// Time `TerminateProcess` gets before the outcome is `Stuck`.
pub const STUCK_AFTER: Duration = Duration::from_secs(2);
/// Granularity of the grace countdown.
const SLICE: Duration = Duration::from_secs(1);

/// Stops `target`. `blocked` = the policy refused it (hard block): returns `Protected` without
/// touching the process. Soft blocks are confirmed by the caller before calling this.
pub fn kill(
    platform: &mut dyn Platform,
    target: ProcKey,
    opts: KillOpts,
    blocked: bool,
    progress: &mut dyn FnMut(Progress),
) -> Outcome {
    if blocked {
        return Outcome::Protected;
    }
    let p = match platform.pin(target, Access::KILL) {
        Ok(p) => p,
        Err(PinError::Gone | PinError::Reused) => return Outcome::Gone,
        Err(PinError::Denied | PinError::Os(_)) => return Outcome::Denied,
    };
    if !opts.force {
        let strategy = platform.strategy(&p);
        if strategy != Strategy::None && platform.graceful(&p, &strategy).is_ok() {
            let mut left = opts.grace;
            while !left.is_zero() {
                progress(Progress::Closing {
                    strategy: strategy.clone(),
                    left,
                });
                let slice = left.min(SLICE);
                if platform.wait_exits(&[&p], slice)[0] {
                    return Outcome::Exited;
                }
                left = left.saturating_sub(slice);
            }
        }
    }
    progress(Progress::Forcing);
    if platform.terminate(&p).is_err() {
        // It may have exited on its own in the meantime; otherwise the OS refused.
        return if platform.wait_exits(&[&p], Duration::ZERO)[0] {
            Outcome::Exited
        } else {
            Outcome::Denied
        };
    }
    if platform.wait_exits(&[&p], STUCK_AFTER)[0] {
        Outcome::Terminated
    } else {
        Outcome::Stuck
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc, Reaction};

    const T: ProcKey = ProcKey {
        pid: 10,
        create_time: 1,
    };

    fn fake(reaction: Reaction, strategy: Strategy) -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![RawProc::new(T, 0, "node.exe")]);
        let mut out = Vec::new();
        f.scan(&mut out).unwrap();
        f.set_reaction(T, reaction);
        f.set_strategy(T, strategy);
        f
    }

    fn run(f: &mut FakePlatform, opts: KillOpts) -> (Outcome, Vec<Progress>) {
        let mut seen = Vec::new();
        let o = kill(f, T, opts, false, &mut |p| seen.push(p));
        (o, seen)
    }

    #[test]
    fn graceful_exit_is_exited() {
        let mut f = fake(Reaction::HonoursGraceful, Strategy::ConsoleCtrlC);
        let (o, seen) = run(&mut f, KillOpts::default());
        assert_eq!(o, Outcome::Exited);
        assert_eq!(
            seen.first(),
            Some(&Progress::Closing {
                strategy: Strategy::ConsoleCtrlC,
                left: Duration::from_secs(5)
            })
        );
        assert!(!seen.contains(&Progress::Forcing));
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn ignored_graceful_escalates_to_terminated() {
        let mut f = fake(Reaction::IgnoresGraceful, Strategy::Window);
        let (o, seen) = run(&mut f, KillOpts::default());
        assert_eq!(o, Outcome::Terminated);
        assert_eq!(seen.last(), Some(&Progress::Forcing));
        // The countdown ticked 5, 4, 3, 2, 1 before forcing.
        let closing = seen
            .iter()
            .filter(|p| matches!(p, Progress::Closing { .. }))
            .count();
        assert_eq!(closing, 5);
    }

    #[test]
    fn force_skips_graceful() {
        let mut f = fake(Reaction::HonoursGraceful, Strategy::ConsoleCtrlC);
        let opts = KillOpts {
            force: true,
            ..KillOpts::default()
        };
        let (o, seen) = run(&mut f, opts);
        assert_eq!(o, Outcome::Terminated);
        assert_eq!(seen, vec![Progress::Forcing]);
        assert_eq!(f.graceful_calls(), 0);
    }

    #[test]
    fn no_strategy_forces_immediately() {
        let mut f = fake(Reaction::IgnoresGraceful, Strategy::None);
        let (o, seen) = run(&mut f, KillOpts::default());
        assert_eq!(o, Outcome::Terminated);
        assert_eq!(seen, vec![Progress::Forcing]);
    }

    #[test]
    fn survives_terminate_is_stuck() {
        let mut f = fake(Reaction::Unkillable, Strategy::None);
        assert_eq!(run(&mut f, KillOpts::default()).0, Outcome::Stuck);
    }

    #[test]
    fn terminate_access_denied_is_denied() {
        let mut f = fake(Reaction::IgnoresGraceful, Strategy::None);
        f.deny(T);
        assert_eq!(run(&mut f, KillOpts::default()).0, Outcome::Denied);
    }

    #[test]
    fn failed_graceful_request_still_escalates() {
        let mut f = fake(Reaction::GracefulFails, Strategy::ConsoleCtrlC);
        assert_eq!(run(&mut f, KillOpts::default()).0, Outcome::Terminated);
    }

    #[test]
    fn already_gone_or_reused_is_gone() {
        let mut f = fake(Reaction::IgnoresGraceful, Strategy::None);
        f.exit(T);
        assert_eq!(run(&mut f, KillOpts::default()).0, Outcome::Gone);
        let mut g = FakePlatform::new(1);
        g.push_frame(vec![RawProc::new(
            ProcKey {
                pid: 10,
                create_time: 9,
            },
            0,
            "other.exe",
        )]);
        let mut out = Vec::new();
        g.scan(&mut out).unwrap();
        assert_eq!(run(&mut g, KillOpts::default()).0, Outcome::Gone);
        assert_eq!(
            g.terminate_calls(),
            0,
            "never touch the process that reused the PID"
        );
    }

    #[test]
    fn blocked_target_is_protected_and_untouched() {
        let mut f = fake(Reaction::IgnoresGraceful, Strategy::None);
        let o = kill(&mut f, T, KillOpts::default(), true, &mut |_| {});
        assert_eq!(o, Outcome::Protected);
        assert_eq!(f.terminate_calls() + f.graceful_calls(), 0);
    }
}
