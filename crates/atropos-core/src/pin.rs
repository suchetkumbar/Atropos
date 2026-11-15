//! Pinned processes (spec §4.4–4.6): an action never addresses a bare PID. It opens a handle,
//! re-checks the create time on that handle, and acts only through it, so a reused PID can never
//! redirect the action to another process.

use crate::{CoreError, ProcKey};

/// What a pin may do. Query and wait are always granted; destructive rights are opt-in, so a
/// dry run (`Access::QUERY`) never even asks for `PROCESS_TERMINATE`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Access {
    pub terminate: bool,
    pub suspend: bool,
    /// Change priority, affinity, efficiency mode (`PROCESS_SET_INFORMATION`).
    pub tune: bool,
}

impl Access {
    pub const QUERY: Access = Access {
        terminate: false,
        suspend: false,
        tune: false,
    };
    pub const KILL: Access = Access {
        terminate: true,
        suspend: true,
        tune: false,
    };
    pub const SUSPEND: Access = Access {
        terminate: false,
        suspend: true,
        tune: false,
    };
}

/// Why a pin failed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PinError {
    /// The PID now belongs to a different (younger) process.
    #[error("PID reused by another process")]
    Reused,
    /// No process has that PID any more.
    #[error("process exited")]
    Gone,
    /// The OS refused the requested access.
    #[error("access denied")]
    Denied,
    #[error(transparent)]
    Os(#[from] CoreError),
}

/// The OS object behind a pin.
#[derive(Debug)]
pub(crate) enum PinHandle {
    /// Fake backend: the key is the identity.
    Fake,
    #[cfg(windows)]
    Windows(crate::platform::windows::OwnedHandle),
}

/// A Job Object that contains the members of a subtree kill (spec §4.6 step 4).
#[derive(Debug)]
pub struct Job {
    pub(crate) handle: JobHandle,
}

#[derive(Debug)]
pub(crate) enum JobHandle {
    Fake(usize),
    #[cfg(windows)]
    Windows(crate::platform::windows::OwnedHandle),
}

/// A process held by an open handle whose create time matched the key. Dropping it closes the
/// handle.
#[derive(Debug)]
pub struct Pinned {
    key: ProcKey,
    access: Access,
    pub(crate) handle: PinHandle,
}

impl Pinned {
    pub(crate) fn new(key: ProcKey, access: Access, handle: PinHandle) -> Self {
        Self {
            key,
            access,
            handle,
        }
    }

    #[must_use]
    pub fn key(&self) -> ProcKey {
        self.key
    }

    #[must_use]
    pub fn access(&self) -> Access {
        self.access
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::platform::{FakePlatform, Platform, RawProc};
    use std::time::Duration;

    fn k(pid: u32, t: u64) -> ProcKey {
        ProcKey {
            pid,
            create_time: t,
        }
    }

    fn fake_with(procs: Vec<RawProc>) -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(procs);
        let mut out = Vec::new();
        f.scan(&mut out).unwrap();
        f
    }

    #[test]
    fn pin_current_process_succeeds() {
        let mut f = fake_with(vec![RawProc::new(k(10, 1), 0, "a.exe")]);
        let p = f.pin(k(10, 1), Access::KILL).unwrap();
        assert_eq!(p.key(), k(10, 1));
        assert_eq!(p.access(), Access::KILL);
    }

    #[test]
    fn pid_reused_between_scan_and_pin_is_reused() {
        // The UI saw pid 10 @ t1; by the time we act, pid 10 belongs to a process born at t7.
        let mut f = fake_with(vec![RawProc::new(k(10, 7), 0, "other.exe")]);
        assert_eq!(f.pin(k(10, 1), Access::KILL).unwrap_err(), PinError::Reused);
    }

    #[test]
    fn pin_of_absent_pid_is_gone_and_denied_is_denied() {
        let mut f = fake_with(vec![RawProc::new(k(11, 1), 0, "svc.exe")]);
        assert_eq!(f.pin(k(10, 1), Access::QUERY).unwrap_err(), PinError::Gone);
        f.deny(k(11, 1));
        assert_eq!(f.pin(k(11, 1), Access::KILL).unwrap_err(), PinError::Denied);
    }

    #[test]
    fn fake_exit_is_seen_by_wait() {
        let mut f = fake_with(vec![
            RawProc::new(k(10, 1), 0, "a.exe"),
            RawProc::new(k(12, 2), 0, "b.exe"),
        ]);
        let a = f.pin(k(10, 1), Access::QUERY).unwrap();
        let b = f.pin(k(12, 2), Access::QUERY).unwrap();
        assert_eq!(f.wait_exits(&[&a, &b], Duration::ZERO), vec![false, false]);
        f.exit(k(12, 2));
        assert_eq!(f.wait_exits(&[&a, &b], Duration::ZERO), vec![false, true]);
    }
}
