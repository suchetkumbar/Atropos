//! The row model behind the table: filter, sort, flat or tree, collapse. Pure (no egui).

use std::cmp::Ordering;
use std::collections::HashSet;

use atropos_core::ProcKey;
use atropos_core::collector::Frame;
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

/// View options the user controls.
#[derive(Debug, Clone, Default)]
pub struct ViewOpts {
    pub tree: bool,
    /// CPU % relative to one core (htop-style) instead of the whole machine.
    pub per_core: bool,
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
pub fn subject<'a>(p: &'a Proc, s: Option<&'a Statics>, ports: Option<&'a [u16]>) -> Subject<'a> {
    Subject {
        pid: p.raw.key.pid,
        image: &p.raw.image,
        label: None,
        cmdline: s.and_then(|s| s.cmdline.ok()).map(|c| &**c),
        user: s.and_then(|s| s.user.ok()).map(|u| &**u),
        cwd: None,
        ports,
    }
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
        SortKey::Name => a.raw.image.to_lowercase().cmp(&b.raw.image.to_lowercase()),
        SortKey::Pid => a.raw.key.pid.cmp(&b.raw.key.pid),
        SortKey::Cpu if tree => a.rollup.cpu_pct.total_cmp(&b.rollup.cpu_pct),
        SortKey::Cpu => f32_key(a.cpu_pct).total_cmp(&f32_key(b.cpu_pct)),
        SortKey::Memory if tree => a.rollup.private_ws.cmp(&b.rollup.private_ws),
        SortKey::Memory => a.raw.private_ws.cmp(&b.raw.private_ws),
        SortKey::Threads => a.raw.threads.cmp(&b.raw.threads),
        SortKey::Handles => a.raw.handles.cmp(&b.raw.handles),
        SortKey::DiskRead => f32_key(a.read_rate).total_cmp(&f32_key(b.read_rate)),
        SortKey::DiskWrite => f32_key(a.write_rate).total_cmp(&f32_key(b.write_rate)),
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
                filter.matches(&subject(p, st, own), me, m).is_some()
            })
            .collect()
    });
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

    if !opts.tree {
        let mut idx: Vec<usize> = (0..procs.len())
            .filter(|&i| matched.as_ref().is_none_or(|m| m[i]))
            .collect();
        sort(&mut idx);
        return idx.into_iter().map(|i| row(i, 0, false)).collect();
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
    use crate::test_frame::{frame, k};

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
}
