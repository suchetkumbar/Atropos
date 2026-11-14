//! Protection policy (spec §1 safety, Phase 2): what may be acted on, and what needs the user to
//! type the process name first. Hard blocks have no override.

use crate::ports::ServiceInfo;
use crate::snapshot::Snapshot;
use crate::statics::{Protection, StaticsMap};
use crate::{Field, ProcKey};

/// What the user wants to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionKind {
    Kill,
    KillTree,
    Suspend,
    Resume,
}

impl ActionKind {
    pub const ALL: [ActionKind; 4] = [
        ActionKind::Kill,
        ActionKind::KillTree,
        ActionKind::Suspend,
        ActionKind::Resume,
    ];
}

/// Why an action is refused outright.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HardReason {
    /// PID 0 / PID 4.
    SystemPid,
    /// A critical Windows process (hard list).
    Critical(String),
    /// Atropos itself, or a process Atropos descends from.
    SelfOrAncestor,
    /// A protected (PPL or full) process.
    Protected,
}

/// Why an action needs the user to type the name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SoftReason {
    /// `dwm.exe`, `explorer.exe`, `sihost.exe`: the desktop itself.
    Shell(String),
    /// A `svchost.exe` hosting several services.
    MultiServiceHost(usize),
    /// A process in another session (another user, or services).
    OtherSession(u32),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Verdict {
    Allowed,
    Soft(SoftReason),
    Hard(HardReason),
}

/// Hard-blocked image names (case-insensitive), spec §1.
pub const HARD_NAMES: [&str; 9] = [
    "Registry",
    "Memory Compression",
    "smss.exe",
    "csrss.exe",
    "wininit.exe",
    "winlogon.exe",
    "services.exe",
    "lsass.exe",
    "fontdrvhost.exe",
];

/// Soft-blocked shell processes.
pub const SHELL_NAMES: [&str; 3] = ["dwm.exe", "explorer.exe", "sihost.exe"];

/// Everything the policy looks at.
pub struct Context<'a> {
    pub snapshot: &'a Snapshot,
    pub statics: &'a StaticsMap,
    pub services: &'a [ServiceInfo],
    /// Atropos's own process.
    pub me: ProcKey,
}

/// Decides whether `action` on `target` is allowed, needs confirmation, or is refused.
#[must_use]
pub fn check(ctx: &Context<'_>, target: ProcKey, action: ActionKind) -> Verdict {
    let snap = ctx.snapshot;
    if target.pid == 0 || target.pid == 4 {
        return Verdict::Hard(HardReason::SystemPid);
    }
    // Atropos and its ancestors, walked over create-time-validated parent links.
    if target == ctx.me {
        return Verdict::Hard(HardReason::SelfOrAncestor);
    }
    let mut up = snap.index.get(&ctx.me).copied();
    while let Some(i) = up {
        if snap.procs[i].raw.key == target {
            return Verdict::Hard(HardReason::SelfOrAncestor);
        }
        up = snap.procs[i].parent;
    }
    let Some(p) = snap.get(target) else {
        // Unknown to this snapshot: the pin decides (Gone / Reused).
        return Verdict::Allowed;
    };
    let image = &*p.raw.image;
    if let Some(name) = HARD_NAMES.iter().find(|n| n.eq_ignore_ascii_case(image)) {
        return Verdict::Hard(HardReason::Critical((*name).to_owned()));
    }
    if let Some(Field::Ok(Protection::Light | Protection::Full)) =
        ctx.statics.get(&target).map(|s| &s.protection)
    {
        return Verdict::Hard(HardReason::Protected);
    }
    if action == ActionKind::Resume {
        return Verdict::Allowed;
    }
    if let Some(name) = SHELL_NAMES.iter().find(|n| n.eq_ignore_ascii_case(image)) {
        return Verdict::Soft(SoftReason::Shell((*name).to_owned()));
    }
    if image.eq_ignore_ascii_case("svchost.exe") {
        let hosted = ctx.services.iter().filter(|s| s.pid == target.pid).count();
        if hosted > 1 {
            return Verdict::Soft(SoftReason::MultiServiceHost(hosted));
        }
    }
    if let Some(mine) = snap.get(ctx.me).map(|m| m.raw.session) {
        if p.raw.session != mine {
            return Verdict::Soft(SoftReason::OtherSession(p.raw.session));
        }
    }
    Verdict::Allowed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{RawProc, SystemStats};
    use crate::snapshot::build;
    use crate::statics::Statics;
    use std::sync::Arc;

    fn k(pid: u32, t: u64) -> ProcKey {
        ProcKey {
            pid,
            create_time: t,
        }
    }

    fn proc_(pid: u32, parent: u32, t: u64, name: &str, session: u32) -> RawProc {
        let mut r = RawProc::new(k(pid, t), parent, name);
        r.session = session;
        r
    }

    struct World {
        snap: Snapshot,
        statics: StaticsMap,
        services: Vec<ServiceInfo>,
    }

    impl World {
        fn new(procs: Vec<RawProc>) -> Self {
            Self {
                snap: build(None, procs, SystemStats::default()),
                statics: StaticsMap::new(),
                services: Vec::new(),
            }
        }

        fn check(&self, me: ProcKey, target: ProcKey, action: ActionKind) -> Verdict {
            let ctx = Context {
                snapshot: &self.snap,
                statics: &self.statics,
                services: &self.services,
                me,
            };
            check(&ctx, target, action)
        }
    }

    /// explorer(1) → terminal(2) → atropos(3), plus unrelated node(9); all in session 1.
    fn desktop() -> World {
        World::new(vec![
            proc_(1, 0, 1, "explorer.exe", 1),
            proc_(2, 1, 2, "WindowsTerminal.exe", 1),
            proc_(3, 2, 3, "Atropos.exe", 1),
            proc_(9, 1, 9, "node.exe", 1),
        ])
    }

    const ME: ProcKey = ProcKey {
        pid: 3,
        create_time: 3,
    };

    #[test]
    fn every_hard_listed_name_is_refused_for_every_action() {
        for (i, name) in HARD_NAMES.iter().enumerate() {
            let pid = 100 + u32::try_from(i).unwrap();
            let w = World::new(vec![
                proc_(3, 0, 3, "Atropos.exe", 1),
                proc_(pid, 0, 1, name, 1),
            ]);
            for action in ActionKind::ALL {
                assert_eq!(
                    w.check(ME, k(pid, 1), action),
                    Verdict::Hard(HardReason::Critical((*name).to_owned())),
                    "{name} {action:?}"
                );
            }
        }
        let upper = World::new(vec![
            proc_(3, 0, 3, "Atropos.exe", 1),
            proc_(7, 0, 1, "LSASS.EXE", 1),
        ]);
        assert!(matches!(
            upper.check(ME, k(7, 1), ActionKind::Kill),
            Verdict::Hard(_)
        ));
    }

    #[test]
    fn system_pids_are_refused() {
        let w = World::new(vec![
            proc_(0, 0, 0, "System Idle Process", 0),
            proc_(4, 0, 1, "System", 0),
            proc_(3, 0, 3, "Atropos.exe", 1),
        ]);
        for action in ActionKind::ALL {
            assert_eq!(
                w.check(ME, k(0, 0), action),
                Verdict::Hard(HardReason::SystemPid)
            );
            assert_eq!(
                w.check(ME, k(4, 1), action),
                Verdict::Hard(HardReason::SystemPid)
            );
        }
    }

    #[test]
    fn own_process_and_ancestors_are_refused() {
        let w = desktop();
        for target in [k(3, 3), k(2, 2), k(1, 1)] {
            for action in ActionKind::ALL {
                assert_eq!(
                    w.check(ME, target, action),
                    Verdict::Hard(HardReason::SelfOrAncestor),
                    "{target:?} {action:?}"
                );
            }
        }
        assert_eq!(w.check(ME, k(9, 9), ActionKind::Kill), Verdict::Allowed);
    }

    #[test]
    fn a_younger_process_reusing_a_parent_pid_is_not_an_ancestor() {
        // Atropos's recorded parent PID 2 now belongs to a process born after Atropos.
        let w = World::new(vec![
            proc_(2, 0, 50, "notepad.exe", 1),
            proc_(3, 2, 3, "Atropos.exe", 1),
        ]);
        assert_eq!(w.check(ME, k(2, 50), ActionKind::Kill), Verdict::Allowed);
    }

    #[test]
    fn protected_processes_are_refused() {
        let mut w = desktop();
        w.statics.insert(
            k(9, 9),
            Arc::new(Statics {
                protection: Field::Ok(Protection::Light),
                ..Statics::unavailable(&Field::Unsupported)
            }),
        );
        assert_eq!(
            w.check(ME, k(9, 9), ActionKind::Kill),
            Verdict::Hard(HardReason::Protected)
        );
    }

    #[test]
    fn shell_processes_need_confirmation_except_resume() {
        let w = World::new(vec![
            proc_(3, 0, 3, "Atropos.exe", 1),
            proc_(20, 0, 1, "explorer.exe", 1),
            proc_(21, 0, 1, "DWM.exe", 1),
        ]);
        assert_eq!(
            w.check(ME, k(20, 1), ActionKind::Kill),
            Verdict::Soft(SoftReason::Shell("explorer.exe".into()))
        );
        assert!(matches!(
            w.check(ME, k(21, 1), ActionKind::Suspend),
            Verdict::Soft(_)
        ));
        assert_eq!(w.check(ME, k(20, 1), ActionKind::Resume), Verdict::Allowed);
    }

    #[test]
    fn multi_service_svchost_needs_confirmation() {
        let mut w = World::new(vec![
            proc_(3, 0, 3, "Atropos.exe", 1),
            proc_(30, 0, 1, "svchost.exe", 1),
            proc_(31, 0, 1, "svchost.exe", 1),
        ]);
        let svc = |pid, name: &str| ServiceInfo {
            pid,
            name: Arc::from(name),
            display: Arc::from(name),
        };
        w.services = vec![svc(30, "a"), svc(30, "b"), svc(31, "c")];
        assert_eq!(
            w.check(ME, k(30, 1), ActionKind::Kill),
            Verdict::Soft(SoftReason::MultiServiceHost(2))
        );
        assert_eq!(w.check(ME, k(31, 1), ActionKind::Kill), Verdict::Allowed);
    }

    #[test]
    fn other_session_needs_confirmation() {
        let w = World::new(vec![
            proc_(3, 0, 3, "Atropos.exe", 1),
            proc_(40, 0, 1, "node.exe", 2),
        ]);
        assert_eq!(
            w.check(ME, k(40, 1), ActionKind::KillTree),
            Verdict::Soft(SoftReason::OtherSession(2))
        );
    }

    #[test]
    fn unknown_target_is_allowed_to_proceed_to_pin() {
        // Not in the snapshot (e.g. typed PID in the CLI): the pin decides (Gone/Reused).
        let w = desktop();
        assert_eq!(w.check(ME, k(777, 7), ActionKind::Kill), Verdict::Allowed);
    }
}
