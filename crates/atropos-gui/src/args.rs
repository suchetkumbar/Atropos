//! GUI command-line flags (perf/diagnostics, and `--lock` for the Explorer menu; no clap in the
//! GUI to keep it small).

use std::ffi::OsString;
use std::path::PathBuf;

/// Parsed GUI flags. Unknown flags are ignored so a stray argument never prevents startup.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Args {
    /// `--startup-log <path>`: append `first_frame_ms=<f64>` after the first frame.
    pub startup_log: Option<PathBuf>,
    /// `--exit-after-first-frame`: close right after the first frame (perf runs).
    pub exit_after_first_frame: bool,
    /// `--perf-log <path>`: append `tick_ms=<f32> scan_ms=<f32>` per collector tick and `frame_ms=<f64>` per UI
    /// update.
    pub perf_log: Option<PathBuf>,
    /// `--lock <path>`: open the palette on "what has this open?" (the Explorer menu entry).
    pub lock: Option<PathBuf>,
}

impl Args {
    /// Parses flags (without the program name).
    pub fn parse(args: impl IntoIterator<Item = OsString>) -> Self {
        let mut out = Self::default();
        let mut it = args.into_iter();
        while let Some(arg) = it.next() {
            match arg.to_str() {
                Some("--startup-log") => out.startup_log = it.next().map(PathBuf::from),
                Some("--exit-after-first-frame") => out.exit_after_first_frame = true,
                Some("--perf-log") => out.perf_log = it.next().map(PathBuf::from),
                Some("--lock") => out.lock = it.next().map(PathBuf::from),
                _ => {}
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn a(v: &[&str]) -> Vec<std::ffi::OsString> {
        v.iter().map(Into::into).collect()
    }

    #[test]
    fn parses_startup_flags_with_unicode_path() {
        let got = Args::parse(a(&[
            "--startup-log",
            r"C:\Tëmp dir\a.log",
            "--exit-after-first-frame",
        ]));
        assert_eq!(got.startup_log, Some(PathBuf::from(r"C:\Tëmp dir\a.log")));
        assert!(got.exit_after_first_frame);
    }

    #[test]
    fn parses_perf_log() {
        let got = Args::parse(a(&["--perf-log", r"C:\p ü\perf.log"]));
        assert_eq!(got.perf_log, Some(PathBuf::from(r"C:\p ü\perf.log")));
    }

    #[test]
    fn unknown_and_dangling_flags_are_ignored() {
        let got = Args::parse(a(&["--bogus", "--startup-log"]));
        assert_eq!(got, Args::default());
    }

    #[test]
    fn parses_lock_path_from_explorer() {
        let got = Args::parse(a(&["--lock", r"D:\Prôjects\my app\data.db"]));
        assert_eq!(got.lock, Some(PathBuf::from(r"D:\Prôjects\my app\data.db")));
    }
}
