//! Parent links (spec §4.2): a recorded parent PID is trusted only if that process is older than
//! the child, which rejects links to a process that reused the parent's PID.

use std::collections::HashMap;

use crate::platform::RawProc;

/// For each process, the index of its validated parent, or `None` for a root.
/// PID 0 (System Idle Process) is never a parent.
#[must_use]
pub fn link<S: std::hash::BuildHasher>(
    procs: &[RawProc],
    by_pid: &HashMap<u32, usize, S>,
) -> Vec<Option<usize>> {
    procs
        .iter()
        .enumerate()
        .map(|(i, child)| {
            let &j = by_pid.get(&child.ppid)?;
            let parent = &procs[j];
            (j != i && parent.key.pid != 0 && parent.key.create_time < child.key.create_time)
                .then_some(j)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ProcKey;

    fn p(pid: u32, parent: u32, t: u64) -> RawProc {
        RawProc::new(
            ProcKey {
                pid,
                create_time: t,
            },
            parent,
            "x.exe",
        )
    }

    fn run(procs: &[RawProc]) -> Vec<Option<usize>> {
        let by_pid: HashMap<u32, usize> = procs
            .iter()
            .enumerate()
            .map(|(i, p)| (p.key.pid, i))
            .collect();
        link(procs, &by_pid)
    }

    #[test]
    fn older_parent_is_linked() {
        assert_eq!(run(&[p(10, 0, 1), p(11, 10, 2)]), vec![None, Some(0)]);
    }

    #[test]
    fn parent_younger_than_child_is_not_a_parent() {
        assert_eq!(run(&[p(10, 0, 5), p(11, 10, 2)]), vec![None, None]);
    }

    #[test]
    fn same_create_time_is_not_a_parent() {
        assert_eq!(run(&[p(10, 0, 5), p(11, 10, 5)]), vec![None, None]);
    }

    #[test]
    fn pid_reuse_orphan_becomes_root() {
        // The real parent of 11 (pid 10, t=1) exited; a new process reused pid 10 at t=9.
        assert_eq!(run(&[p(10, 3, 9), p(11, 10, 2)]), vec![None, None]);
    }

    #[test]
    fn idle_is_never_a_parent_and_self_parent_is_root() {
        assert_eq!(
            run(&[p(0, 0, 0), p(4, 0, 1), p(7, 7, 3)]),
            vec![None, None, None]
        );
    }

    #[test]
    fn missing_parent_is_root() {
        assert_eq!(run(&[p(11, 99, 2)]), vec![None]);
    }
}
