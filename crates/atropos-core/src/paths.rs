//! Well-known per-user directories. Pure `std`; no unsafe.

use std::ffi::OsString;
use std::path::PathBuf;

/// `%LOCALAPPDATA%\Atropos\logs`, or `<temp>\Atropos\logs` when the variable is missing or empty.
#[must_use]
pub fn log_dir() -> PathBuf {
    log_dir_from(|k| std::env::var_os(k))
}

/// [`log_dir`] with an injectable environment lookup (for tests).
pub fn log_dir_from(get: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let root = get("LOCALAPPDATA")
        .filter(|v| !v.is_empty())
        .map_or_else(std::env::temp_dir, PathBuf::from);
    root.join("Atropos").join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    #[test]
    fn log_dir_uses_localappdata() {
        let root = r"C:\Users\José Müller\AppData\Local";
        let d = log_dir_from(|k| (k == "LOCALAPPDATA").then(|| OsString::from(root)));
        assert!(d.starts_with(root));
        assert!(d.ends_with(Path::new("Atropos").join("logs")));
    }

    #[test]
    fn log_dir_falls_back_to_temp() {
        let d = log_dir_from(|_| None);
        assert!(d.starts_with(std::env::temp_dir()));
        assert!(d.ends_with(Path::new("Atropos").join("logs")));
    }

    #[test]
    fn log_dir_ignores_empty_localappdata() {
        let d = log_dir_from(|_| Some(OsString::new()));
        assert!(d.starts_with(std::env::temp_dir()));
    }
}
