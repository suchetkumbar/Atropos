//! Well-known per-user directories, by each OS's convention. Pure `std`; no unsafe.
//!
//! | | data (logs, history, journal) | settings |
//! |---|---|---|
//! | Windows | `%LOCALAPPDATA%\Atropos` | `%APPDATA%\Atropos` |
//! | macOS | `~/Library/Application Support/Atropos` | same |
//! | Linux | `$XDG_STATE_HOME/atropos` (`~/.local/state/atropos`) | `$XDG_CONFIG_HOME/atropos` (`~/.config/atropos`) |
//!
//! When the variables are missing or empty, `<temp>/Atropos` (a portable copy still runs).

use std::ffi::OsString;
use std::path::PathBuf;

/// Which convention to follow (a parameter, so every OS's rules are tested everywhere).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Os {
    Windows,
    MacOs,
    Linux,
}

/// The OS this was built for (other Unixes follow Linux).
pub const OS: Os = if cfg!(windows) {
    Os::Windows
} else if cfg!(target_os = "macos") {
    Os::MacOs
} else {
    Os::Linux
};

fn var(get: &impl Fn(&str) -> Option<OsString>, k: &str) -> Option<PathBuf> {
    get(k).filter(|v| !v.is_empty()).map(PathBuf::from)
}

fn fallback() -> PathBuf {
    std::env::temp_dir().join("Atropos")
}

/// Where logs, the kill history and the suspend journal live.
pub fn data_dir_for(os: Os, get: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let dir = match os {
        Os::Windows => var(&get, "LOCALAPPDATA").map(|d| d.join("Atropos")),
        Os::MacOs => var(&get, "HOME").map(|h| h.join("Library/Application Support/Atropos")),
        Os::Linux => var(&get, "XDG_STATE_HOME")
            .or_else(|| var(&get, "HOME").map(|h| h.join(".local/state")))
            .map(|d| d.join("atropos")),
    };
    dir.unwrap_or_else(fallback)
}

/// Where `config.toml` lives.
pub fn config_dir_for(os: Os, get: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    let dir = match os {
        Os::Windows => var(&get, "APPDATA").map(|d| d.join("Atropos")),
        Os::MacOs => var(&get, "HOME").map(|h| h.join("Library/Application Support/Atropos")),
        Os::Linux => var(&get, "XDG_CONFIG_HOME")
            .or_else(|| var(&get, "HOME").map(|h| h.join(".config")))
            .map(|d| d.join("atropos")),
    };
    dir.unwrap_or_else(fallback)
}

/// This user's data folder.
#[must_use]
pub fn data_dir() -> PathBuf {
    data_dir_for(OS, |k| std::env::var_os(k))
}

/// The log folder (`<data>/logs`).
#[must_use]
pub fn log_dir() -> PathBuf {
    log_dir_from(|k| std::env::var_os(k))
}

/// [`log_dir`] with an injectable environment lookup (for tests).
pub fn log_dir_from(get: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    data_dir_for(OS, get).join("logs")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;

    fn env(pairs: &'static [(&'static str, &'static str)]) -> impl Fn(&str) -> Option<OsString> {
        move |k| {
            pairs
                .iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| OsString::from(v))
        }
    }

    #[test]
    fn windows_uses_localappdata_and_appdata() {
        let get = env(&[
            ("LOCALAPPDATA", r"C:\Users\José Müller\AppData\Local"),
            ("APPDATA", r"C:\Users\José Müller\AppData\Roaming"),
        ]);
        assert_eq!(
            data_dir_for(Os::Windows, &get),
            Path::new(r"C:\Users\José Müller\AppData\Local").join("Atropos")
        );
        assert_eq!(
            config_dir_for(Os::Windows, &get),
            Path::new(r"C:\Users\José Müller\AppData\Roaming").join("Atropos")
        );
    }

    #[test]
    fn linux_follows_xdg_with_home_fallbacks() {
        let xdg = env(&[
            ("XDG_STATE_HOME", "/home/jö/state"),
            ("XDG_CONFIG_HOME", "/home/jö/cfg"),
            ("HOME", "/home/jö"),
        ]);
        assert_eq!(
            data_dir_for(Os::Linux, &xdg),
            Path::new("/home/jö/state/atropos")
        );
        assert_eq!(
            config_dir_for(Os::Linux, &xdg),
            Path::new("/home/jö/cfg/atropos")
        );
        let home = env(&[("HOME", "/home/jö"), ("XDG_STATE_HOME", "")]);
        assert_eq!(
            data_dir_for(Os::Linux, &home),
            Path::new("/home/jö/.local/state/atropos")
        );
        assert_eq!(
            config_dir_for(Os::Linux, &home),
            Path::new("/home/jö/.config/atropos")
        );
    }

    #[test]
    fn macos_uses_application_support() {
        let get = env(&[("HOME", "/Users/jö")]);
        let want = Path::new("/Users/jö/Library/Application Support/Atropos");
        assert_eq!(data_dir_for(Os::MacOs, &get), want);
        assert_eq!(config_dir_for(Os::MacOs, &get), want);
    }

    #[test]
    fn missing_or_empty_variables_fall_back_to_temp() {
        for os in [Os::Windows, Os::MacOs, Os::Linux] {
            assert_eq!(
                data_dir_for(os, |_| None),
                std::env::temp_dir().join("Atropos")
            );
            assert_eq!(
                config_dir_for(os, |_| Some(OsString::new())),
                std::env::temp_dir().join("Atropos")
            );
        }
    }

    #[test]
    fn the_log_dir_is_inside_the_data_dir() {
        let d = log_dir_from(|_| None);
        assert!(d.starts_with(std::env::temp_dir()));
        assert!(d.ends_with(Path::new("Atropos").join("logs")));
    }
}
