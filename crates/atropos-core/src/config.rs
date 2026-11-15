//! Settings (spec Phase 3): `%APPDATA%\Atropos\config.toml`. Created only when the user changes
//! a setting (Z3). Unknown keys are ignored; a corrupt or newer file falls back to defaults with
//! one notice, never a crash.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub const SCHEMA_VERSION: u32 = 1;
/// The global hotkey that opens the command palette (spec Phase 6).
pub const DEFAULT_HOTKEY: &str = "Ctrl+Alt+K";

/// When the kill confirmation dialog appears.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Confirm {
    /// Every kill asks first (default).
    #[default]
    Always,
    /// Only soft-blocked targets (explorer, other sessions, …) ask; hard blocks always refuse.
    Risky,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Theme {
    #[default]
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum View {
    #[default]
    Flat,
    Tree,
}

/// All user settings.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub schema_version: u32,
    /// Refresh interval in seconds (0.5–10); `None` = adaptive (1 / 2 / 5 s).
    pub tick_secs: Option<f32>,
    /// Graceful-stop grace period, seconds.
    pub grace_secs: u32,
    pub confirm: Confirm,
    /// Extra environment-variable name fragments to mask (added to the built-in list).
    pub mask_names: Vec<String>,
    pub theme: Theme,
    pub default_view: View,
    /// Global hotkey for the command palette, e.g. `Ctrl+Alt+K`.
    pub hotkey: String,
    /// Extra smart-label rules (`[[labels]]`), tried before the built-in ones (spec §4.8).
    /// Kept last: TOML writes arrays of tables after the plain keys.
    pub labels: Vec<crate::labels::RuleDef>,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            schema_version: SCHEMA_VERSION,
            tick_secs: None,
            grace_secs: 5,
            confirm: Confirm::Always,
            mask_names: Vec::new(),
            theme: Theme::System,
            default_view: View::Flat,
            hotkey: DEFAULT_HOTKEY.to_owned(),
            labels: Vec::new(),
        }
    }
}

/// `config.toml` in this OS's settings folder (`%APPDATA%\Atropos` on Windows, see
/// [`crate::paths`]); the temp folder without the variables.
#[must_use]
pub fn path_from(get: impl Fn(&str) -> Option<OsString>) -> PathBuf {
    crate::paths::config_dir_for(crate::paths::OS, get).join("config.toml")
}

#[must_use]
pub fn path() -> PathBuf {
    path_from(|k| std::env::var_os(k))
}

/// Loads `path`. Returns the config and, when it could not be used as is, a one-line notice.
#[must_use]
pub fn load(path: &Path) -> (Config, Option<String>) {
    let text = match std::fs::read_to_string(path) {
        Ok(t) => t,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return (Config::default(), None),
        Err(e) => {
            return (
                Config::default(),
                Some(format!(
                    "Could not read {} ({e}); using defaults.",
                    path.display()
                )),
            );
        }
    };
    match toml::from_str::<Config>(&text) {
        Ok(c) if c.schema_version > SCHEMA_VERSION => (
            Config::default(),
            Some(format!(
                "{} was written by a newer Atropos; using defaults (the file is left as is).",
                path.display()
            )),
        ),
        Ok(c) => (c, None),
        Err(e) => (
            Config::default(),
            Some(format!(
                "{} is not valid ({}); using defaults.",
                path.display(),
                e.message()
            )),
        ),
    }
}

/// Writes `cfg` to `path` (creating the folder), via a temp file + rename.
///
/// # Errors
/// If the folder or file cannot be written.
pub fn save(path: &Path, cfg: &Config) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let text = toml::to_string_pretty(cfg).map_err(std::io::Error::other)?;
    let tmp = path.with_extension("toml.tmp");
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dir(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("atropos cfg ü {tag} {}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[cfg(windows)]
    #[test]
    fn path_uses_appdata() {
        let p =
            path_from(|k| (k == "APPDATA").then(|| OsString::from(r"C:\Users\Jö\AppData\Roaming")));
        assert_eq!(
            p,
            PathBuf::from(r"C:\Users\Jö\AppData\Roaming")
                .join("Atropos")
                .join("config.toml")
        );
        let fallback = path_from(|_| None);
        assert!(fallback.ends_with(Path::new("Atropos").join("config.toml")));
    }

    #[test]
    fn missing_file_gives_defaults_and_creates_nothing() {
        let d = dir("missing");
        let (c, notice) = load(&d.join("config.toml"));
        assert_eq!(c, Config::default());
        assert_eq!(notice, None);
        assert!(!d.exists(), "first run creates no files (Z3)");
    }

    #[test]
    fn round_trip_through_save() {
        let d = dir("roundtrip");
        let p = d.join("config.toml");
        let c = Config {
            tick_secs: Some(2.5),
            grace_secs: 9,
            confirm: Confirm::Risky,
            mask_names: vec!["PRIVATE".into()],
            theme: Theme::Dark,
            default_view: View::Tree,
            hotkey: "Win+Shift+K".into(),
            ..Config::default()
        };
        save(&p, &c).unwrap();
        assert_eq!(load(&p), (c, None));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn partial_and_unknown_keys() {
        let d = dir("partial");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.toml");
        std::fs::write(
            &p,
            "grace_secs = 12\nfuture_feature = true\n[unknown]\nx = 1\n",
        )
        .unwrap();
        let (c, notice) = load(&p);
        assert_eq!(c.grace_secs, 12);
        assert_eq!(c.theme, Theme::System);
        assert_eq!(c.hotkey, DEFAULT_HOTKEY);
        assert_eq!(notice, None);
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn corrupt_file_falls_back_with_a_notice() {
        let d = dir("corrupt");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.toml");
        for bad in [
            "grace_secs = \"soon\"",
            "this is [[ not toml",
            "theme = \"purple\"",
        ] {
            std::fs::write(&p, bad).unwrap();
            let (c, notice) = load(&p);
            assert_eq!(c, Config::default(), "{bad}");
            assert!(notice.is_some_and(|n| n.contains("defaults")), "{bad}");
        }
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn newer_schema_falls_back_with_a_notice() {
        let d = dir("newer");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.toml");
        std::fs::write(&p, "schema_version = 99\ngrace_secs = 3\n").unwrap();
        let (c, notice) = load(&p);
        assert_eq!(c, Config::default());
        assert!(notice.is_some_and(|n| n.contains("newer")));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn user_label_rules_load_and_round_trip() {
        let d = dir("labels");
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("config.toml");
        std::fs::write(
            &p,
            "grace_secs = 7

[[labels]]
image = [\"node\"]
cmdline = \"api/server.js\"
label = \"Orders API · {project}\"

[[labels]]
image = [\"python\"]
markers = [\"manage.py\"]
label = \"Django\"
",
        )
        .unwrap();
        let (c, notice) = load(&p);
        assert_eq!(notice, None);
        assert_eq!(c.grace_secs, 7);
        assert_eq!(c.labels.len(), 2);
        assert_eq!(c.labels[0].cmdline.as_deref(), Some("api/server.js"));
        assert_eq!(c.labels[1].markers, vec!["manage.py".to_owned()]);
        save(&p, &c).unwrap();
        assert_eq!(load(&p), (c, None));
        let _ = std::fs::remove_dir_all(&d);
    }
}
