//! "What is locking this file?" (Restart Manager) on the real backend, against a `file-locker`
//! this test spawned itself. The lookup only reads; nothing is ended.
#![cfg(windows)]
#![allow(clippy::unwrap_used)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use atropos_core::platform::Platform;
use atropos_core::platform::windows::WindowsPlatform;

/// A fresh folder with a space and non-ASCII characters in its name.
fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("atropos lock tëst {tag} {}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn lock(path: &Path) -> atropos_fixtures::SpawnedFixture {
    let child = atropos_fixtures::spawn(
        "file-locker",
        &["--path", path.to_str().unwrap(), "--secs", "60"],
    )
    .unwrap();
    assert_eq!(
        child
            .next_line(Duration::from_secs(5))
            .as_deref()
            .map(str::trim),
        Some("locked")
    );
    child
}

#[test]
fn the_process_holding_a_file_is_found_by_key() {
    let d = dir("file");
    let f = d.join("dätä file.txt");
    let child = lock(&f);
    let got = WindowsPlatform::new().file_lockers(&f).unwrap();
    assert!(got.contains(&child.key), "{got:?} lacks {:?}", child.key);
    drop(child);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_long_path_form_finds_it_too() {
    let d = dir("long");
    let f = d.join("x.txt");
    let child = lock(&f);
    let long = PathBuf::from(format!(r"\\?\{}", f.display()));
    let got = WindowsPlatform::new().file_lockers(&long).unwrap();
    assert!(got.contains(&child.key), "{got:?}");
    drop(child);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_folder_reports_what_holds_files_inside_it() {
    let d = dir("folder");
    std::fs::create_dir_all(d.join("sub")).unwrap();
    let child = lock(&d.join("sub").join("deep.txt"));
    let got = WindowsPlatform::new().file_lockers(&d).unwrap();
    assert!(got.contains(&child.key), "{got:?}");
    drop(child);
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_free_file_has_no_lockers() {
    let d = dir("free");
    let f = d.join("free.txt");
    std::fs::write(&f, "x").unwrap();
    assert_eq!(WindowsPlatform::new().file_lockers(&f).unwrap(), Vec::new());
    let _ = std::fs::remove_dir_all(&d);
}

#[test]
fn a_missing_path_is_a_clear_error() {
    let e = WindowsPlatform::new()
        .file_lockers(Path::new(r"C:\definitely\not\here\x.txt"))
        .unwrap_err();
    assert_eq!(e, atropos_core::CoreError::NotFound);
    assert!(e.to_string().contains("does not exist"), "{e}");
}
