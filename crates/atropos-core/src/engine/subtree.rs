//! Subtree termination (spec §4.6): collect → pin + freeze top-down → settle (≤ 5 rounds) →
//! contain in a Job Object → graceful → force → report. Members left alive are resumed so
//! nothing stays frozen.

use std::collections::HashSet;
use std::fmt::Write as _;
use std::time::Duration;

use super::kill::{KillOpts, Outcome, Progress, STUCK_AFTER, Strategy};
use crate::ProcKey;
use crate::pin::{Access, PinError, Pinned};
use crate::platform::Platform;
use crate::snapshot::{Snapshot, build};

/// Settle rounds (spec: at most 5).
pub const SETTLE_ROUNDS: usize = 5;

/// One outcome per member, in collection order (root first).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TreeReport {
    pub members: Vec<(ProcKey, Outcome)>,
}

impl TreeReport {
    #[must_use]
    pub fn count(&self, o: Outcome) -> usize {
        self.members.iter().filter(|(_, m)| *m == o).count()
    }

    /// `11 exited · 1 terminated · 0 stuck · 1 protected (skipped)`; denied/gone/protected
    /// only when non-zero.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut s = format!(
            "{} exited · {} terminated · {} stuck",
            self.count(Outcome::Exited),
            self.count(Outcome::Terminated),
            self.count(Outcome::Stuck)
        );
        for (o, label) in [
            (Outcome::Denied, "denied"),
            (Outcome::Gone, "gone"),
            (Outcome::Protected, "protected (skipped)"),
        ] {
            let n = self.count(o);
            if n > 0 {
                let _ = write!(s, " · {n} {label}");
            }
        }
        s
    }
}

/// One subtree member while the algorithm runs.
struct Member {
    key: ProcKey,
    pin: Option<Pinned>,
    /// Suspended by us and not yet resumed.
    frozen: bool,
    in_job: bool,
    outcome: Option<Outcome>,
}

impl Member {
    fn alive(&self) -> bool {
        self.outcome.is_none() && self.pin.is_some()
    }
}

/// Pre-order (parents first) keys of the subtree at `root`.
#[must_use]
pub fn preorder(snap: &Snapshot, root: usize) -> Vec<ProcKey> {
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(i) = stack.pop() {
        out.push(snap.procs[i].raw.key);
        stack.extend(snap.children[i].iter().rev());
    }
    out
}

/// Adds a member: policy check, pin, freeze.
fn admit(
    platform: &mut dyn Platform,
    key: ProcKey,
    blocked: &dyn Fn(ProcKey) -> bool,
    members: &mut Vec<Member>,
    known: &mut HashSet<ProcKey>,
) {
    known.insert(key);
    let mut m = Member {
        key,
        pin: None,
        frozen: false,
        in_job: false,
        outcome: None,
    };
    if blocked(key) {
        m.outcome = Some(Outcome::Protected);
    } else {
        match platform.pin(key, Access::KILL) {
            Ok(p) => {
                m.frozen = platform.suspend(&p).is_ok();
                m.pin = Some(p);
            }
            Err(PinError::Gone | PinError::Reused) => m.outcome = Some(Outcome::Gone),
            Err(PinError::Denied | PinError::Os(_)) => m.outcome = Some(Outcome::Denied),
        }
    }
    members.push(m);
}

fn scan(platform: &mut dyn Platform) -> Option<Snapshot> {
    let mut raw = Vec::new();
    let stats = platform.scan(&mut raw).ok()?;
    Some(build(None, raw, stats))
}

/// Waits until every live member exited or `budget` slices of `slice` ran out; marks exits
/// with `outcome`. Returns early when everyone is gone.
fn wait_all(
    platform: &mut dyn Platform,
    members: &mut [Member],
    slice: Duration,
    mut budget: Duration,
    outcome: Outcome,
    tick: &mut dyn FnMut(Duration),
) {
    loop {
        let idx: Vec<usize> = (0..members.len()).filter(|&i| members[i].alive()).collect();
        if idx.is_empty() || budget.is_zero() {
            return;
        }
        tick(budget);
        let step = budget.min(slice);
        let pins: Vec<&Pinned> = idx
            .iter()
            .filter_map(|&i| members[i].pin.as_ref())
            .collect();
        let exited = platform.wait_exits(&pins, step);
        let mut any = false;
        for (n, &i) in idx.iter().enumerate() {
            if exited.get(n).copied().unwrap_or(false) {
                members[i].outcome = Some(outcome);
                any = true;
            }
        }
        if !any {
            budget = budget.saturating_sub(step);
        }
    }
}

/// Steps 1–3: collect the subtree, pin + freeze top-down, then settle (children born since).
/// `None` when the root is not running.
fn collect(
    platform: &mut dyn Platform,
    root: ProcKey,
    blocked: &dyn Fn(ProcKey) -> bool,
) -> Option<Vec<Member>> {
    let snap = scan(platform)?;
    let &ri = snap.index.get(&root)?;
    let mut members = Vec::new();
    let mut known = HashSet::new();
    for key in preorder(&snap, ri) {
        admit(platform, key, blocked, &mut members, &mut known);
    }
    for _ in 0..SETTLE_ROUNDS {
        let Some(s) = scan(platform) else { break };
        let new: Vec<ProcKey> = s
            .procs
            .iter()
            .filter(|p| !known.contains(&p.raw.key))
            .filter(|p| {
                p.parent
                    .is_some_and(|i| known.contains(&s.procs[i].raw.key))
            })
            .map(|p| p.raw.key)
            .collect();
        if new.is_empty() {
            break;
        }
        for key in new {
            admit(platform, key, blocked, &mut members, &mut known);
        }
    }
    Some(members)
}

/// Step 5: resume everyone, send each its graceful request, wait up to the grace period.
fn graceful_phase(
    platform: &mut dyn Platform,
    members: &mut [Member],
    grace: Duration,
    progress: &mut dyn FnMut(Progress),
) {
    for m in members.iter_mut() {
        if let (Some(p), true) = (&m.pin, m.frozen) {
            m.frozen = platform.resume(p).is_err();
        }
    }
    let mut shown = Strategy::None;
    for m in members.iter() {
        if let Some(p) = m.pin.as_ref().filter(|_| m.outcome.is_none()) {
            let s = platform.strategy(p);
            if s != Strategy::None && platform.graceful(p, &s).is_ok() && shown == Strategy::None {
                shown = s;
            }
        }
    }
    if shown != Strategy::None {
        let mut tick = |left| {
            progress(Progress::Closing {
                strategy: shown.clone(),
                left,
            });
        };
        wait_all(
            platform,
            members,
            Duration::from_secs(1),
            grace,
            Outcome::Exited,
            &mut tick,
        );
    }
}

/// Step 6: terminate the job, then any member outside it; wait for the exits.
fn force_phase(
    platform: &mut dyn Platform,
    members: &mut [Member],
    job: Option<&crate::pin::Job>,
    progress: &mut dyn FnMut(Progress),
) {
    if !members.iter().any(Member::alive) {
        return;
    }
    progress(Progress::Forcing);
    let job_ok = job.is_some_and(|j| platform.terminate_job(j).is_ok());
    let mut refused = Vec::new();
    for (i, m) in members.iter().enumerate() {
        if let Some(p) = m.pin.as_ref().filter(|_| m.outcome.is_none()) {
            if !(job_ok && m.in_job) && platform.terminate(p).is_err() {
                refused.push(i);
            }
        }
    }
    wait_all(
        platform,
        members,
        STUCK_AFTER,
        STUCK_AFTER,
        Outcome::Terminated,
        &mut |_| {},
    );
    for i in refused {
        if members[i].outcome.is_none() {
            members[i].outcome = Some(Outcome::Denied);
        }
    }
}

/// Stops `root` and every descendant. `blocked(key)` = policy hard block for that member.
pub fn kill_tree(
    platform: &mut dyn Platform,
    root: ProcKey,
    opts: KillOpts,
    blocked: &dyn Fn(ProcKey) -> bool,
    progress: &mut dyn FnMut(Progress),
) -> TreeReport {
    if blocked(root) {
        return TreeReport {
            members: vec![(root, Outcome::Protected)],
        };
    }
    let Some(mut members) = collect(platform, root, blocked) else {
        return TreeReport {
            members: vec![(root, Outcome::Gone)],
        };
    };
    // 4. Contain: children spawned from here on land in the job.
    let job = platform.create_job().ok();
    if let Some(job) = &job {
        for m in &mut members {
            if let Some(p) = &m.pin {
                m.in_job = platform.assign(job, p).is_ok();
            }
        }
    }
    if !opts.force {
        graceful_phase(platform, &mut members, opts.grace, progress);
    }
    force_phase(platform, &mut members, job.as_ref(), progress);
    // 7. Never leave a survivor frozen; report.
    for m in &members {
        if let (Some(p), true, None) = (&m.pin, m.frozen, m.outcome) {
            let _ = platform.resume(p);
        }
    }
    TreeReport {
        members: members
            .into_iter()
            .map(|m| (m.key, m.outcome.unwrap_or(Outcome::Stuck)))
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, RawProc, Reaction};

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    fn p(pid: u32, parent: u32) -> RawProc {
        RawProc::new(k(pid), parent, "node.exe")
    }

    fn force() -> KillOpts {
        KillOpts {
            force: true,
            ..KillOpts::default()
        }
    }

    fn alive(f: &mut FakePlatform) -> Vec<u32> {
        let mut out = Vec::new();
        f.scan(&mut out).unwrap();
        out.iter().map(|r| r.key.pid).collect()
    }

    #[test]
    fn graceful_tree_all_exit() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10)]);
        for key in [k(10), k(11)] {
            f.set_reaction(key, Reaction::HonoursGraceful);
            f.set_strategy(key, Strategy::ConsoleCtrlC);
        }
        let r = kill_tree(&mut f, k(10), KillOpts::default(), &|_| false, &mut |_| {});
        assert_eq!(
            r.members,
            vec![(k(10), Outcome::Exited), (k(11), Outcome::Exited)]
        );
        assert_eq!(r.summary(), "2 exited · 0 terminated · 0 stuck");
    }

    #[test]
    fn fork_storm_during_settle_is_caught_frozen_and_killed() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10)]);
        f.push_frame(vec![p(10, 1), p(11, 10), p(12, 11)]);
        f.push_frame(vec![p(10, 1), p(11, 10), p(12, 11), p(13, 12)]);
        f.push_frame(vec![p(10, 1), p(11, 10), p(12, 11), p(13, 12), p(50, 1)]);
        let r = kill_tree(&mut f, k(10), force(), &|_| false, &mut |_| {});
        let keys: Vec<u32> = r.members.iter().map(|(k, _)| k.pid).collect();
        assert_eq!(keys, [10, 11, 12, 13]);
        assert!(r.members.iter().all(|(_, o)| *o == Outcome::Terminated));
        // Frozen top-down before anything was killed.
        assert_eq!(f.suspend_log(), &[k(10), k(11), k(12), k(13)]);
        assert_eq!(alive(&mut f), [50], "only the unrelated process survives");
    }

    #[test]
    fn child_spawned_after_resume_dies_with_the_job() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1)]);
        f.set_strategy(k(10), Strategy::ConsoleCtrlC);
        f.spawn_on_resume(k(10), p(77, 10));
        let r = kill_tree(&mut f, k(10), KillOpts::default(), &|_| false, &mut |_| {});
        assert_eq!(r.members, vec![(k(10), Outcome::Terminated)]);
        assert!(
            !alive(&mut f).contains(&77),
            "the late child was in the job"
        );
    }

    #[test]
    fn protected_member_is_skipped_but_its_siblings_die() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10), p(12, 10)]);
        let r = kill_tree(&mut f, k(10), force(), &|key| key == k(11), &mut |_| {});
        assert_eq!(
            r.members,
            vec![
                (k(10), Outcome::Terminated),
                (k(11), Outcome::Protected),
                (k(12), Outcome::Terminated)
            ]
        );
        assert_eq!(alive(&mut f), [11]);
        assert_eq!(
            r.summary(),
            "0 exited · 2 terminated · 0 stuck · 1 protected (skipped)"
        );
    }

    #[test]
    fn blocked_root_is_refused_untouched() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10)]);
        let r = kill_tree(&mut f, k(10), force(), &|key| key == k(10), &mut |_| {});
        assert_eq!(r.members, vec![(k(10), Outcome::Protected)]);
        assert_eq!(f.suspend_log(), &[] as &[ProcKey]);
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn missing_root_is_gone() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(11, 1)]);
        let r = kill_tree(&mut f, k(10), force(), &|_| false, &mut |_| {});
        assert_eq!(r.members, vec![(k(10), Outcome::Gone)]);
    }

    #[test]
    fn stuck_survivors_are_resumed() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10)]);
        f.set_reaction(k(11), Reaction::Unkillable);
        let r = kill_tree(&mut f, k(10), force(), &|_| false, &mut |_| {});
        assert_eq!(r.members[1], (k(11), Outcome::Stuck));
        assert_eq!(f.suspend_count(k(11)), 0, "never leave a process frozen");
    }

    #[test]
    fn denied_member_is_reported() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![p(10, 1), p(11, 10)]);
        f.deny(k(11));
        let r = kill_tree(&mut f, k(10), force(), &|_| false, &mut |_| {});
        assert_eq!(r.members[1], (k(11), Outcome::Denied));
        assert_eq!(r.summary(), "0 exited · 1 terminated · 0 stuck · 1 denied");
    }

    #[test]
    fn summary_matches_the_spec_example() {
        let mut members = vec![(k(1), Outcome::Exited); 11];
        members.push((k(2), Outcome::Terminated));
        members.push((k(3), Outcome::Protected));
        assert_eq!(
            TreeReport { members }.summary(),
            "11 exited · 1 terminated · 0 stuck · 1 protected (skipped)"
        );
    }
}
