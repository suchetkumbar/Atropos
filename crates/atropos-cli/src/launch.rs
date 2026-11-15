//! `atr` with no arguments: start the sibling GUI (`Atropos.exe`, `Atropos` on Unix), detached.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The GUI's file name on this OS.
#[must_use]
pub fn gui_name() -> String {
    format!("Atropos{}", std::env::consts::EXE_SUFFIX)
}

/// Path of the GUI that ships beside `atr`.
pub fn gui_path(atr_exe: &Path) -> PathBuf {
    atr_exe.with_file_name(gui_name())
}

/// The GUI next to `atr`: `Atropos`, or `atropos` as Linux packages install it.
#[must_use]
pub fn find_gui(atr_exe: &Path) -> Option<PathBuf> {
    [
        gui_path(atr_exe),
        atr_exe.with_file_name(format!("atropos{}", std::env::consts::EXE_SUFFIX)),
    ]
    .into_iter()
    .find(|p| p.is_file())
}

/// Detaches the GUI from this terminal: no inherited console (Windows) and its own process
/// group, so Ctrl+C here does not reach it.
fn detach(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// `DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP`.
        const FLAGS: u32 = 0x0000_0008 | 0x0000_0200;
        cmd.creation_flags(FLAGS);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt as _;
        cmd.process_group(0);
    }
}

/// Starts `Atropos.exe` from the same folder as `atr_exe` and returns without waiting.
///
/// # Errors
/// If the GUI is not next to `atr.exe`, or it cannot be started.
pub fn launch_gui(atr_exe: &Path) -> anyhow::Result<()> {
    let Some(gui) = find_gui(atr_exe) else {
        let dir = atr_exe.parent().unwrap_or(atr_exe);
        anyhow::bail!("{} not found next to {}", gui_name(), dir.display());
    };
    let mut cmd = Command::new(&gui);
    detach(&mut cmd);
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| anyhow::anyhow!("could not start {}: {e}", gui.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn gui_path_handles_unicode_and_spaces() {
        let dir = Path::new("José Müller").join("My Tools");
        let p = gui_path(&dir.join(format!("atr{}", std::env::consts::EXE_SUFFIX)));
        assert_eq!(p, dir.join(gui_name()));
        #[cfg(windows)]
        assert_eq!(
            gui_path(Path::new(r"C:\Users\José Müller\My Tools\atr.exe")),
            Path::new(r"C:\Users\José Müller\My Tools\Atropos.exe")
        );
    }

    #[test]
    fn a_packaged_lowercase_gui_is_found_too() {
        let dir = std::env::temp_dir().join(format!("atropos-launch-lc-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let atr = dir.join(format!("atr{}", std::env::consts::EXE_SUFFIX));
        let lower = dir.join(format!("atropos{}", std::env::consts::EXE_SUFFIX));
        std::fs::write(&lower, b"").unwrap();
        // Case-insensitive file systems (Windows, default macOS) find either spelling.
        assert!(find_gui(&atr).is_some());
        let _ = std::fs::remove_dir_all(&dir);
        assert_eq!(find_gui(&atr), None);
    }

    #[test]
    fn launch_gui_reports_missing_exe() {
        let dir = std::env::temp_dir().join("atropos-launch-test-ü dir");
        let err = launch_gui(&dir.join("atr.exe")).unwrap_err().to_string();
        assert!(
            err.contains(&format!("{} not found next to", gui_name())),
            "{err}"
        );
    }
}
