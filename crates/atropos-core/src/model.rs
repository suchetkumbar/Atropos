//! Data model: process identity and per-field availability.

use std::fmt;

/// Stable identity of a process: PID plus creation time (FILETIME, 100 ns since 1601 UTC).
///
/// A PID alone is never used to act on a process: PIDs are reused, `(pid, create_time)` is not.
#[derive(Copy, Clone, Hash, Eq, PartialEq, Ord, PartialOrd, Debug)]
pub struct ProcKey {
    pub pid: u32,
    pub create_time: u64,
}

impl fmt::Display for ProcKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}@{}", self.pid, self.create_time)
    }
}

/// A value that may be unavailable for a specific, explainable reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Field<T> {
    /// The value was read.
    Ok(T),
    /// Access was denied (for example a protected or elevated process).
    Restricted,
    /// The process exited while the value was being read.
    Gone,
    /// Not available on this platform or OS version.
    Unsupported,
}

impl<T> Field<T> {
    /// The value, if it was read.
    #[must_use]
    pub fn ok(&self) -> Option<&T> {
        match self {
            Field::Ok(v) => Some(v),
            _ => None,
        }
    }

    /// Whether the value was read.
    #[must_use]
    pub fn is_ok(&self) -> bool {
        matches!(self, Field::Ok(_))
    }

    /// Transforms the value, preserving the reason when it is unavailable.
    pub fn map<U>(self, f: impl FnOnce(T) -> U) -> Field<U> {
        match self {
            Field::Ok(v) => Field::Ok(f(v)),
            Field::Restricted => Field::Restricted,
            Field::Gone => Field::Gone,
            Field::Unsupported => Field::Unsupported,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prockey_same_pid_different_time_are_distinct() {
        let a = ProcKey {
            pid: 42,
            create_time: 1,
        };
        let b = ProcKey {
            pid: 42,
            create_time: 2,
        };
        assert_ne!(a, b);
        let set: std::collections::HashSet<_> = [a, b].into_iter().collect();
        assert_eq!(set.len(), 2);
    }

    #[test]
    fn prockey_display() {
        assert_eq!(
            ProcKey {
                pid: 42,
                create_time: 7
            }
            .to_string(),
            "42@7"
        );
    }

    #[test]
    fn field_map_preserves_non_ok() {
        assert_eq!(Field::<u32>::Restricted.map(|v| v + 1), Field::Restricted);
        assert_eq!(Field::Ok(1).map(|v| v + 1), Field::Ok(2));
        assert_eq!(Field::<u32>::Gone.ok(), None);
        assert!(Field::Ok(()).is_ok());
        assert!(!Field::<()>::Unsupported.is_ok());
    }
}
