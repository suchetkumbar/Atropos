//! The row model behind the table: filter, sort, flat or tree, collapse. Pure (no egui).

use std::cmp::Ordering;
use std::collections::HashSet;
use std::sync::Arc;

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
use atropos_core::context::Ctx;
use atropos_core::filter::{Filter, FilterMatcher, Subject};
use atropos_core::snapshot::Proc;
use atropos_core::statics::Statics;

/// Sortable columns, in display order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Name,
    Pid,
    Cpu,
    Memory,
    Threads,
    Handles,
    DiskRead,
    DiskWrite,
    Gpu,
    GpuMem,
    User,
    Started,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Sort {
    pub key: SortKey,
    pub descending: bool,
}

impl Default for Sort {
    /// CPU, highest first: what is busy is what a developer looks for.
    fn default() -> Self {
        Self {
            key: SortKey::Cpu,
            descending: true,
        }
    }
}

/// Which processes the table lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Scope {
    #[default]
    All,
    /// Processes whose working directory is in a git repository, grouped by repository.
    Projects,
    /// Orphaned dev servers (spec Phase 4): listening, in a repository, parent gone.
    Leftovers,
    /// The treemap (spec Phase 5) instead of the table; every process.
    Blast,
    /// Processes doing disk I/O, busiest first, with bars (spec Phase 5).
    Disk,
}

/// View options the user controls.
#[derive(Debug, Clone, Default)]
pub struct ViewOpts {
    pub scope: Scope,
    /// In [`Scope::Projects`]: show only this repository (its root path).
    pub project: Option<Arc<str>>,
    /// Tree layout (only in [`Scope::All`]).
    pub tree: bool,
    /// CPU % relative to one core (htop-style) instead of the whole machine.
    pub per_core: bool,
    /// Show the GPU columns (the collector samples GPU counters only while they are shown).
    pub gpu: bool,
    pub sort: Sort,
    /// Tree nodes the user collapsed (everything is expanded by default).
    pub collapsed: HashSet<ProcKey>,
}

/// One visible row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Row {
    /// Index into `frame.snapshot.procs`.
    pub idx: usize,
    pub key: ProcKey,
    /// Indent level (0 in flat mode).
    pub depth: u16,
    pub has_children: bool,
    /// Collapsed tree node: its CPU and memory cells show the subtree rollup.
    pub collapsed: bool,
}

/// The filter text as seen by [`Filter`], for one process.
#[must_use]
pub fn subject<'a>(
    p: &'a Proc,
    s: Option<&'a Statics>,
    ctx: Option<&'a Ctx>,
    ports: Option<&'a [u16]>,
) -> Subject<'a> {
    Subject {
        pid: p.raw.key.pid,
        image: &p.raw.image,
        label: ctx.and_then(|c| c.label.as_deref()),
        cmdline: s.and_then(|s| s.cmdline.ok()).map(|c| &**c),
        user: s.and_then(|s| s.user.ok()).map(|u| &**u),
        cwd: ctx.and_then(|c| c.cwd.as_deref()),
        ports,
    }
}

/// What the Name column shows: the smart label when there is one, else the image name.
#[must_use]
pub fn display_name<'a>(frame: &'a Frame, p: &'a Proc) -> &'a str {
    frame
        .context
        .get(&p.raw.key)
        .and_then(|c| c.label.as_deref())
        .unwrap_or(&p.raw.image)
}

/// Root of the repository a process is in.
fn repo_of<'a>(frame: &'a Frame, p: &Proc) -> Option<&'a Arc<str>> {
    frame.context.get(&p.raw.key).and_then(|c| c.repo.as_ref())
}

/// GPU figures of a process this frame; `None` when GPU counters were not sampled.
pub fn gpu_of(frame: &Frame, p: &Proc) -> Option<atropos_core::gpu::GpuUsage> {
    frame
        .gpu
        .as_ref()
        .map(|g| g.get(&p.raw.key).copied().unwrap_or_default())
}

fn user_of(frame: &Frame, p: &Proc) -> String {
    frame
        .statics
        .get(&p.raw.key)
        .and_then(|s| s.user.ok())
        .map(|u| u.to_lowercase())
        .unwrap_or_default()
}

/// Column order for two processes; ties break by PID (ascending) so sorting is stable.
/// In tree mode CPU and memory compare subtree rollups, so busy subtrees rise together.
fn compare(frame: &Frame, sort: Sort, tree: bool, a: &Proc, b: &Proc) -> Ordering {
    let f32_key = |v: Option<f32>| v.unwrap_or(-1.0);
    let primary = match sort.key {
        SortKey::Name => display_name(frame, a)
            .to_lowercase()
            .cmp(&display_name(frame, b).to_lowercase()),
        SortKey::Pid => a.raw.key.pid.cmp(&b.raw.key.pid),
        SortKey::Cpu if tree => a.rollup.cpu_pct.total_cmp(&b.rollup.cpu_pct),
        SortKey::Cpu => f32_key(a.cpu_pct).total_cmp(&f32_key(b.cpu_pct)),
        SortKey::Memory if tree => a.rollup.private_ws.cmp(&b.rollup.private_ws),
        SortKey::Memory => a.raw.private_ws.cmp(&b.raw.private_ws),
        SortKey::Threads => a.raw.threads.cmp(&b.raw.threads),
        SortKey::Handles => a.raw.handles.cmp(&b.raw.handles),
        SortKey::DiskRead => f32_key(a.read_rate).total_cmp(&f32_key(b.read_rate)),
        SortKey::DiskWrite => f32_key(a.write_rate).total_cmp(&f32_key(b.write_rate)),
        SortKey::Gpu => f32_key(gpu_of(frame, a).map(|g| g.util_pct))
            .total_cmp(&f32_key(gpu_of(frame, b).map(|g| g.util_pct))),
        SortKey::GpuMem => gpu_of(frame, a)
            .map(|g| g.dedicated)
            .cmp(&gpu_of(frame, b).map(|g| g.dedicated)),
        SortKey::User => user_of(frame, a).cmp(&user_of(frame, b)),
        SortKey::Started => a.raw.key.create_time.cmp(&b.raw.key.create_time),
    };
    let primary = if sort.descending {
        primary.reverse()
    } else {
        primary
    };
    primary.then(a.raw.key.pid.cmp(&b.raw.key.pid))
}

/// Local ports per PID from this frame's sockets (`None` when sockets were not read).
fn port_map(frame: &Frame) -> Option<std::collections::HashMap<u32, Vec<u16>>> {
    frame.sockets.as_ref().map(|ss| {
        let mut m: std::collections::HashMap<u32, Vec<u16>> = std::collections::HashMap::new();
        for s in ss.iter() {
            let v = m.entry(s.owner).or_default();
            if !v.contains(&s.local.port()) {
                v.push(s.local.port());
            }
        }
        m
    })
}

/// Per process: whether the scope lists it (`None` = no narrowing).
fn in_scope(frame: &Frame, opts: &ViewOpts) -> Option<Vec<bool>> {
    let snap = &frame.snapshot;
    match opts.scope {
        Scope::All | Scope::Blast => None,
        Scope::Projects => Some(
            snap.procs
                .iter()
                .map(|p| {
                    repo_of(frame, p)
                        .is_some_and(|r| opts.project.as_ref().is_none_or(|want| want == r))
                })
                .collect(),
        ),
        Scope::Disk => Some(snap.procs.iter().map(crate::disk::active).collect()),
        Scope::Leftovers => {
            let keys: HashSet<ProcKey> = frame
                .sockets
                .as_ref()
                .map(|s| atropos_core::projects::leftovers(snap, s, &frame.context))
                .unwrap_or_default()
                .into_iter()
                .collect();
            Some(
                snap.procs
                    .iter()
                    .map(|p| keys.contains(&p.raw.key))
                    .collect(),
            )
        }
    }
}

/// Visible rows for `frame`. With a filter in tree mode, ancestors of matches stay visible
/// (expanded) so every match keeps its context.
#[must_use]
pub fn rows(
    frame: &Frame,
    filter: &Filter,
    me: &str,
    opts: &ViewOpts,
    m: &mut FilterMatcher,
) -> Vec<Row> {
    let snap = &frame.snapshot;
    let procs = &snap.procs;
    let ports = port_map(frame);
    let matched: Option<Vec<bool>> = (!filter.is_empty()).then(|| {
        procs
            .iter()
            .map(|p| {
                let st = frame.statics.get(&p.raw.key).map(|s| &**s);
                let own: Option<&[u16]> = ports
                    .as_ref()
                    .map(|m| m.get(&p.raw.key.pid).map_or(&[][..], Vec::as_slice));
                let ctx = frame.context.get(&p.raw.key);
                filter.matches(&subject(p, st, ctx, own), me, m).is_some()
            })
            .collect()
    });
    // The scope narrows the list like a filter does.
    let matched: Option<Vec<bool>> = match (matched, in_scope(frame, opts)) {
        (Some(m), Some(s)) => Some(m.iter().zip(&s).map(|(a, b)| *a && *b).collect()),
        (m, s) => m.or(s),
    };
    let sort = |v: &mut Vec<usize>| {
        v.sort_by(|&a, &b| compare(frame, opts.sort, opts.tree, &procs[a], &procs[b]));
    };
    let row = |i: usize, depth: u16, collapsed: bool| Row {
        idx: i,
        key: procs[i].raw.key,
        depth,
        has_children: opts.tree && !snap.children[i].is_empty(),
        collapsed,
    };

    if !opts.tree || opts.scope != Scope::All {
        let mut idx: Vec<usize> = (0..procs.len())
            .filter(|&i| matched.as_ref().is_none_or(|m| m[i]))
            .collect();
        sort(&mut idx);
        if opts.scope == Scope::Projects {
            // Keep each repository together (stable: the column order holds inside a group).
            idx.sort_by_cached_key(|&i| {
                repo_of(frame, &procs[i]).map(|r| {
                    (
                        atropos_core::labels::project::folder_name(r).to_lowercase(),
                        Arc::clone(r),
                    )
                })
            });
        }
        return idx
            .into_iter()
            .map(|i| Row {
                has_children: false,
                ..row(i, 0, false)
            })
            .collect();
    }

    // Tree: with a filter, a process is visible if it or a descendant matches.
    let visible: Option<Vec<bool>> = matched.map(|m| {
        let mut v = m.clone();
        for (i, hit) in m.iter().enumerate() {
            if *hit {
                let mut up = procs[i].parent;
                while let Some(p) = up {
                    if v[p] && !m[p] {
                        break; // this ancestor chain is already marked
                    }
                    v[p] = true;
                    up = procs[p].parent;
                }
            }
        }
        v
    });
    let shown = |i: usize| visible.as_ref().is_none_or(|v| v[i]);
    let mut out = Vec::with_capacity(procs.len());
    let mut roots: Vec<usize> = snap.roots.iter().copied().filter(|&i| shown(i)).collect();
    sort(&mut roots);
    let mut stack: Vec<(usize, u16)> = roots.into_iter().rev().map(|i| (i, 0)).collect();
    while let Some((i, depth)) = stack.pop() {
        let collapsed = visible.is_none() && opts.collapsed.contains(&procs[i].raw.key);
        out.push(row(i, depth, collapsed && !snap.children[i].is_empty()));
        if collapsed {
            continue;
        }
        let mut kids: Vec<usize> = snap.children[i]
            .iter()
            .copied()
            .filter(|&c| shown(c))
            .collect();
        sort(&mut kids);
        stack.extend(kids.into_iter().rev().map(|c| (c, depth.saturating_add(1))));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_frame::{frame, frame_with_context, k};

    fn run_ctx(q: &str, opts: &ViewOpts) -> Vec<Row> {
        rows(
            &frame_with_context(),
            &Filter::parse(q),
            "josé",
            opts,
            &mut FilterMatcher::default(),
        )
    }

    #[test]
    fn labels_are_searchable_and_sort_as_the_name() {
        assert_eq!(pids(&run_ctx("name:next", &ViewOpts::default())), [2]);
        assert_eq!(pids(&run_ctx("vs code", &ViewOpts::default())), [3]);
        // The image still matches when a label is shown.
        assert_eq!(pids(&run_ctx("name:node", &ViewOpts::default())), [2, 4]);
        let opts = ViewOpts {
            sort: Sort {
                key: SortKey::Name,
                descending: false,
            },
            ..ViewOpts::default()
        };
        // explorer.exe, Next.js dev · shop, node.exe, svchost.exe, VS Code
        assert_eq!(pids(&run_ctx("", &opts)), [1, 2, 4, 9, 3]);
    }

    #[test]
    fn cwd_term_matches_the_working_directory() {
        // Unknown cwds are not excluded (they may match once read); known ones must contain it.
        assert_eq!(pids(&run_ctx("cwd:shop pid:2", &ViewOpts::default())), [2]);
        let elsewhere = pids(&run_ctx("cwd:elsewhere", &ViewOpts::default()));
        assert!(!elsewhere.contains(&2) && !elsewhere.contains(&4));
    }

    fn with_repo_for_9() -> Frame {
        let mut f = frame_with_context();
        let mut ctx = (*f.context).clone();
        ctx.insert(
            k(9),
            Ctx {
                repo: Some(Arc::from(r"E:\Atropos")),
                ..Ctx::default()
            },
        );
        f.context = Arc::new(ctx);
        f
    }

    #[test]
    fn projects_scope_lists_repository_processes_grouped_and_flat() {
        let f = with_repo_for_9();
        let mut opts = ViewOpts {
            scope: Scope::Projects,
            tree: true,
            ..ViewOpts::default()
        };
        let go = |opts: &ViewOpts| {
            rows(
                &f,
                &Filter::default(),
                "josé",
                opts,
                &mut FilterMatcher::default(),
            )
        };
        let r = go(&opts);
        // Atropos (9) before shop (2 and 4, by CPU); never a tree.
        assert_eq!(pids(&r), [9, 2, 4]);
        assert!(r.iter().all(|r| r.depth == 0 && !r.has_children));
        opts.project = Some(Arc::from(r"D:\code\shop"));
        assert_eq!(pids(&go(&opts)), [2, 4]);
    }

    #[test]
    fn leftovers_scope_lists_orphaned_listeners_only() {
        use atropos_core::ports::{Proto, Socket, TcpState};
        let mut f = with_repo_for_9();
        let opts = ViewOpts {
            scope: Scope::Leftovers,
            ..ViewOpts::default()
        };
        let go = |f: &Frame| {
            rows(
                f,
                &Filter::default(),
                "josé",
                &opts,
                &mut FilterMatcher::default(),
            )
        };
        assert!(go(&f).is_empty(), "nothing before sockets are read");
        let listen = |owner, port: u16| Socket {
            proto: Proto::Tcp,
            local: std::net::SocketAddr::from(([127, 0, 0, 1], port)),
            remote: None,
            state: Some(TcpState::Listen),
            owner,
        };
        // 9 has no parent and is in a repo; 4 is in a repo but its parent (2) is alive.
        f.sockets = Some(Arc::new(vec![listen(9, 3000), listen(4, 3001)]));
        assert_eq!(pids(&go(&f)), [9]);
    }

    fn pids(rows: &[Row]) -> Vec<u32> {
        rows.iter().map(|r| r.key.pid).collect()
    }

    fn run(q: &str, opts: &ViewOpts) -> Vec<Row> {
        rows(
            &frame(),
            &Filter::parse(q),
            "josé",
            opts,
            &mut FilterMatcher::default(),
        )
    }

    #[test]
    fn flat_sorted_by_cpu_desc_by_default() {
        assert_eq!(pids(&run("", &ViewOpts::default())), [2, 3, 4, 9, 1]);
    }

    #[test]
    fn sort_by_name_ascending_is_case_insensitive() {
        let opts = ViewOpts {
            sort: Sort {
                key: SortKey::Name,
                descending: false,
            },
            ..ViewOpts::default()
        };
        // Ties (two node.exe) break by PID so the order is stable.
        assert_eq!(pids(&run("", &opts)), [3, 1, 2, 4, 9]);
    }

    #[test]
    fn tree_children_follow_parent_and_siblings_are_sorted() {
        let opts = ViewOpts {
            tree: true,
            ..ViewOpts::default()
        };
        let r = run("", &opts);
        assert_eq!(pids(&r), [1, 2, 4, 3, 9]);
        let depths: Vec<u16> = r.iter().map(|r| r.depth).collect();
        assert_eq!(depths, [0, 1, 2, 1, 0]);
        assert!(r[0].has_children && !r[2].has_children);
    }

    #[test]
    fn collapsed_node_hides_descendants() {
        let mut opts = ViewOpts {
            tree: true,
            ..ViewOpts::default()
        };
        opts.collapsed.insert(k(2));
        let r = run("", &opts);
        assert_eq!(pids(&r), [1, 2, 3, 9]);
        assert!(r[1].collapsed);
    }

    #[test]
    fn filter_flat_and_tree_keeps_ancestors() {
        assert_eq!(pids(&run("name:node", &ViewOpts::default())), [2, 4]);
        let mut opts = ViewOpts {
            tree: true,
            ..ViewOpts::default()
        };
        opts.collapsed.insert(k(2));
        // 4 matches; its ancestors 1 and 2 stay visible and expanded.
        assert_eq!(pids(&run("pid:4", &opts)), [1, 2, 4]);
    }

    #[test]
    fn user_me_excludes_restricted_user() {
        assert_eq!(pids(&run("user:me", &ViewOpts::default())), [2, 3, 4, 1]);
    }

    #[test]
    fn port_filter_uses_the_frames_sockets() {
        use atropos_core::ports::{Proto, Socket};
        let mut f = frame();
        let filter = Filter::parse(":3000");
        let none = rows(
            &f,
            &filter,
            "josé",
            &ViewOpts::default(),
            &mut FilterMatcher::default(),
        );
        assert!(none.is_empty(), "no sockets loaded yet");
        f.sockets = Some(std::sync::Arc::new(vec![Socket {
            proto: Proto::Tcp,
            local: "[::1]:3000".parse().unwrap(),
            remote: None,
            state: None,
            owner: 4,
        }]));
        let got = rows(
            &f,
            &filter,
            "josé",
            &ViewOpts::default(),
            &mut FilterMatcher::default(),
        );
        assert_eq!(pids(&got), [4]);
    }

    #[test]
    fn fuzzy_text_searches_cmdline() {
        assert_eq!(pids(&run("code flag", &ViewOpts::default())), [3]);
    }

    #[test]
    fn sorting_by_gpu_puts_the_busiest_first() {
        use atropos_core::gpu::GpuUsage;
        let mut f = frame();
        f.gpu = Some(std::sync::Arc::new(
            [
                (
                    k(3),
                    GpuUsage {
                        util_pct: 70.0,
                        dedicated: 1,
                    },
                ),
                (
                    k(9),
                    GpuUsage {
                        util_pct: 5.0,
                        dedicated: 900,
                    },
                ),
            ]
            .into_iter()
            .collect(),
        ));
        let order = |key| {
            let opts = ViewOpts {
                sort: Sort {
                    key,
                    descending: true,
                },
                ..ViewOpts::default()
            };
            rows(
                &f,
                &Filter::default(),
                "",
                &opts,
                &mut FilterMatcher::default(),
            )
            .iter()
            .map(|r| r.key.pid)
            .take(2)
            .collect::<Vec<_>>()
        };
        assert_eq!(order(SortKey::Gpu), vec![3, 9]);
        assert_eq!(order(SortKey::GpuMem), vec![9, 3]);
    }
}
