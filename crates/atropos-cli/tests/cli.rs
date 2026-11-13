use std::process::Command;

#[test]
fn version_prints_name_and_version() {
    let out = Command::new(env!("CARGO_BIN_EXE_atr"))
        .arg("--version")
        .output()
        .unwrap();
    assert!(out.status.success());
    assert!(String::from_utf8_lossy(&out.stdout).starts_with("atr "));
}

#[test]
fn missing_gui_exits_1_with_message() {
    let dir = std::env::temp_dir().join(format!("atropos cli test ü {}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let atr = dir.join("atr.exe");
    std::fs::copy(env!("CARGO_BIN_EXE_atr"), &atr).unwrap();
    let out = Command::new(&atr).output().unwrap();
    let _ = std::fs::remove_dir_all(&dir);
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("Atropos.exe not found next to"));
}
