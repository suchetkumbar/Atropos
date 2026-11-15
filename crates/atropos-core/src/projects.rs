//! Project context (spec §4.8, Phase 4): processes grouped by the git repository their working
//! directory is in, the "kill everything from `<repo>`" plan, and leftover dev servers.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::ProcKey;
use crate::context::ContextMap;
use crate::engine::policy::{self, ActionKind, Verdict};
use crate::labels::project::folder_name;
use crate::ports::{Socket, TcpState};
use crate::snapshot::Snapshot;

/// The processes whose working directory is inside one repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Group {
    /// Repository root.
    pub repo: Arc<str>,
    /// Folder name of the root, for display.
    pub name: String,
    /// In snapshot order.
    pub members: Vec<ProcKey>,
}

/// Groups by repository, sorted by name (case-insensitive), then by path.
#[must_use]
pub fn groups(snap: &Snapshot, context: &ContextMap) -> Vec<Group> {
    let mut by_repo: HashMap<&Arc<str>, Vec<ProcKey>> = HashMap::new();
    for p in &snap.procs {
        if let Some(repo) = context.get(&p.raw.key).and_then(|c| c.repo.as_ref()) {
            by_repo.entry(repo).or_default().push(p.raw.key);
        }
    }
    let mut out: Vec<Group> = by_repo
        .into_iter()
        .map(|(repo, members)| Group {
            repo: Arc::clone(repo),
            name: folder_name(repo).to_owned(),
            members,
        })
        .collect();
    out.sort_by(|a, b| {
        a.name
            .to_lowercase()
            .cmp(&b.name.to_lowercase())
            .then_with(|| a.repo.cmp(&b.repo))
    });
    out
}

/// What "kill everything from `<repo>`" will do.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct GroupPlan {
    /// Subtree roots to end (each still goes through the engine's own policy check).
    pub roots: Vec<ProcKey>,
    /// Members left alone, with the reason.
    pub skipped: Vec<(ProcKey, Verdict)>,
}

/// Plans a group kill: every member the policy allows is ended with its subtree; protected and
/// confirmation-only members are skipped. A root is an allowed member with no allowed member
/// above it, so nothing is ended twice.
#[must_use]
pub fn plan_group_kill(ctx: &policy::Context<'_>, members: &[ProcKey]) -> GroupPlan {
    let snap = ctx.snapshot;
    let mut plan = GroupPlan::default();
    let mut allowed: HashSet<ProcKey> = HashSet::new();
    for &key in members {
        if snap.get(key).is_none() {
            continue;
        }
        match policy::check(ctx, key, ActionKind::KillTree) {
            Verdict::Allowed => {
                allowed.insert(key);
            }
            verdict => plan.skipped.push((key, verdict)),
        }
    }
    for &key in members {
        if !allowed.contains(&key) {
            continue;
        }
        let mut up = snap.get(key).and_then(|p| p.parent);
        let mut covered = false;
        while let Some(i) = up {
            if allowed.contains(&snap.procs[i].raw.key) {
                covered = true;
                break;
            }
            up = snap.procs[i].parent;
        }
        if !covered {
            plan.roots.push(key);
        }
    }
    plan
}

/// Orphaned dev servers (spec Phase 4): listening on a TCP port, working directory inside a
/// repository, and no living parent. In snapshot order.
#[must_use]
pub fn leftovers(snap: &Snapshot, sockets: &[Socket], context: &ContextMap) -> Vec<ProcKey> {
    let listeners: HashSet<u32> = sockets
        .iter()
        .filter(|s| s.state == Some(TcpState::Listen))
        .map(|s| s.owner)
        .collect();
    snap.procs
        .iter()
        .filter(|p| p.parent.is_none() && listeners.contains(&p.raw.key.pid))
        .filter(|p| context.get(&p.raw.key).is_some_and(|c| c.repo.is_some()))
        .map(|p| p.raw.key)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Ctx;
    use crate::platform::{RawProc, SystemStats};
    use crate::ports::Proto;
    use crate::snapshot::build;
    use crate::statics::StaticsMap;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    /// `(pid, ppid, image)`; a parent must come with a smaller pid (older create time).
    fn snap(procs: &[(u32, u32, &str)]) -> Snapshot {
        build(
            None,
            procs
                .iter()
                .map(|(pid, ppid, image)| RawProc::new(k(*pid), *ppid, image))
                .collect(),
            SystemStats::default(),
        )
    }

    fn in_repo(pairs: &[(u32, &str)]) -> ContextMap {
        pairs
            .iter()
            .map(|(pid, repo)| {
                (
                    k(*pid),
                    Ctx {
                        repo: Some(Arc::from(*repo)),
                        ..Ctx::default()
                    },
                )
            })
            .collect()
    }

    fn listen(owner: u32, port: u16) -> Socket {
        Socket {
            proto: Proto::Tcp,
            local: format!("127.0.0.1:{port}").parse().unwrap(),
            remote: None,
            state: Some(TcpState::Listen),
            owner,
        }
    }

    #[test]
    fn groups_by_repo_sorted_by_name_on_any_drive_or_share() {
        let s = snap(&[
            (10, 0, "node.exe"),
            (11, 10, "node.exe"),
            (12, 0, "cargo.exe"),
            (13, 0, "python.exe"),
            (14, 0, "svchost.exe"),
        ]);
        let ctx = in_repo(&[
            (10, r"D:\code\shop"),
            (11, r"D:\code\shop"),
            (12, r"\\nas\team\Atropos"),
            (13, r"E:\"),
        ]);
        let g = groups(&s, &ctx);
        let names: Vec<&str> = g.iter().map(|g| g.name.as_str()).collect();
        assert_eq!(names, ["Atropos", "E:", "shop"]);
        assert_eq!(g[2].members, vec![k(10), k(11)]);
        assert_eq!(&*g[0].repo, r"\\nas\team\Atropos");
    }

    #[test]
    fn group_kill_goes_through_the_policy_and_ends_each_tree_once() {
        // 100 = Atropos, started from the shell 50 that sits in the repo.
        let s = snap(&[
            (50, 0, "pwsh.exe"),
            (60, 50, "node.exe"),
            (61, 60, "node.exe"),
            (70, 50, "cmd.exe"),
            (71, 70, "node.exe"),
            (80, 0, "explorer.exe"),
            (100, 50, "Atropos.exe"),
        ]);
        let statics = StaticsMap::new();
        let ctx = policy::Context {
            snapshot: &s,
            statics: &statics,
            services: &[],
            me: k(100),
        };
        let members = [k(50), k(60), k(61), k(71), k(80), k(999)];
        let plan = plan_group_kill(&ctx, &members);
        // 61 is under 60; 71's parent (70) is not a member, so 71 is its own root.
        assert_eq!(plan.roots, vec![k(60), k(71)]);
        let skipped: Vec<ProcKey> = plan.skipped.iter().map(|(key, _)| *key).collect();
        assert_eq!(skipped, vec![k(50), k(80)]);
        assert!(matches!(plan.skipped[0].1, Verdict::Hard(_)));
        assert!(matches!(plan.skipped[1].1, Verdict::Soft(_)));
    }

    #[test]
    fn leftover_is_a_parentless_listener_inside_a_repo() {
        let s = snap(&[
            (10, 9999, "node.exe"), // parent gone, listens, in a repo → leftover
            (20, 0, "pwsh.exe"),
            (21, 20, "node.exe"),       // living parent
            (30, 9999, "node.exe"),     // parent gone, in a repo, not listening
            (40, 9999, "postgres.exe"), // parent gone, listening, no repo
        ]);
        let ctx = in_repo(&[
            (10, r"D:\code\shop"),
            (21, r"D:\code\shop"),
            (30, r"D:\code\shop"),
        ]);
        let mut established = listen(30, 5000);
        established.state = Some(TcpState::Established);
        let sockets = [
            listen(10, 3000),
            listen(21, 3001),
            listen(40, 5432),
            established,
        ];
        assert_eq!(leftovers(&s, &sockets, &ctx), vec![k(10)]);
    }
}
