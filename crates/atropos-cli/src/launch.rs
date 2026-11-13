//! `atr` with no arguments: start the sibling `Atropos.exe`, detached.

use std::os::windows::process::CommandExt as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// `DETACHED_PROCESS`: the GUI does not inherit our console.
const DETACHED_PROCESS: u32 = 0x0000_0008;
/// `CREATE_NEW_PROCESS_GROUP`: Ctrl+C in this terminal does not reach the GUI.
const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;

/// Path of the GUI that ships beside `atr.exe`.
pub fn gui_path(atr_exe: &Path) -> PathBuf {
    atr_exe.with_file_name("Atropos.exe")
}

/// Starts `Atropos.exe` from the same folder as `atr_exe` and returns without waiting.
///
/// # Errors
/// If the GUI is not next to `atr.exe`, or it cannot be started.
pub fn launch_gui(atr_exe: &Path) -> anyhow::Result<()> {
    let gui = gui_path(atr_exe);
    if !gui.is_file() {
        let dir = atr_exe.parent().unwrap_or(atr_exe);
        anyhow::bail!("Atropos.exe not found next to {}", dir.display());
    }
    Command::new(&gui)
        .creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP)
        .stdin(Stdio::null())
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
        let p = gui_path(Path::new(r"C:\Users\José Müller\My Tools\atr.exe"));
        assert_eq!(p, Path::new(r"C:\Users\José Müller\My Tools\Atropos.exe"));
    }

    #[test]
    fn launch_gui_reports_missing_exe() {
        let dir = std::env::temp_dir().join("atropos-launch-test-ü dir");
        let err = launch_gui(&dir.join("atr.exe")).unwrap_err().to_string();
        assert!(err.contains("Atropos.exe not found next to"), "{err}");
    }
}
