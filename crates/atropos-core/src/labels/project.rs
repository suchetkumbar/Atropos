//! What a working directory says about its process: marker files, the project name, and the
//! repository root (spec §4.8). Runs off the collector thread (see `context`).

use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;

/// Manifests larger than this are not read (the folder name is used instead).
const MAX_MANIFEST: u64 = 1024 * 1024;
/// Directory entries examined for a `*.ext` marker.
const MAX_ENTRIES: usize = 4096;

/// Facts about one directory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct DirInfo {
    /// The requested markers (lower-case, as asked) that exist in the directory.
    pub markers: Vec<String>,
    /// `name` from `package.json` or `Cargo.toml`, else the folder name.
    pub project: Option<Arc<str>>,
    /// Nearest ancestor (or the directory itself) that contains `.git`.
    pub repo: Option<Arc<str>>,
}

fn has_suffix(dir: &Path, suffix: &str) -> bool {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return false;
    };
    entries.take(MAX_ENTRIES).flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .to_lowercase()
            .ends_with(suffix)
    })
}

/// Probes `dir` for `markers` (lower-case; `*.ext` = any file with that suffix).
#[must_use]
pub fn probe(dir: &Path, markers: &[String]) -> DirInfo {
    let found = markers
        .iter()
        .filter(|m| match m.strip_prefix('*') {
            Some(suffix) => has_suffix(dir, suffix),
            None => dir.join(m.as_str()).exists(),
        })
        .cloned()
        .collect();
    DirInfo {
        markers: found,
        project: project_name(dir).map(Arc::from),
        repo: repo_root(dir).map(|p| Arc::from(p.to_string_lossy().as_ref())),
    }
}

fn read_capped(path: &Path) -> Option<String> {
    let file = std::fs::File::open(path).ok()?;
    if file.metadata().ok()?.len() > MAX_MANIFEST {
        return None;
    }
    let mut text = String::new();
    file.take(MAX_MANIFEST).read_to_string(&mut text).ok()?;
    Some(text)
}

#[derive(Deserialize)]
struct Named {
    name: Option<String>,
}

#[derive(Deserialize)]
struct CargoManifest {
    package: Option<Named>,
}

fn non_empty(name: Option<String>) -> Option<String> {
    name.map(|n| n.trim().to_owned()).filter(|n| !n.is_empty())
}

/// `name` from `package.json`, then `Cargo.toml`, else the folder name. Nothing else is kept
/// from either file.
#[must_use]
pub fn project_name(dir: &Path) -> Option<String> {
    let from_package = || {
        let text = read_capped(&dir.join("package.json"))?;
        non_empty(serde_json::from_str::<Named>(&text).ok()?.name)
    };
    let from_cargo = || {
        let text = read_capped(&dir.join("Cargo.toml"))?;
        non_empty(toml::from_str::<CargoManifest>(&text).ok()?.package?.name)
    };
    from_package().or_else(from_cargo).or_else(|| {
        let name = folder_name(dir.to_str()?);
        (!name.is_empty()).then(|| name.to_owned())
    })
}

/// The nearest directory at or above `dir` that contains `.git` (a folder, or a file as in
/// worktrees and submodules).
#[must_use]
pub fn repo_root(dir: &Path) -> Option<PathBuf> {
    // `ancestors` of a path with a trailing separator starts at the same directory.
    let trimmed = dir.components().collect::<PathBuf>();
    trimmed
        .ancestors()
        .find(|a| a.join(".git").exists())
        .map(Path::to_path_buf)
}

/// The last path component, for display ("shop" for `D:\code\shop\`).
#[must_use]
pub fn folder_name(path: &str) -> &str {
    let trimmed = path.trim_end_matches(['\\', '/']);
    trimmed.rsplit(['\\', '/']).next().unwrap_or(trimmed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Tmp(PathBuf);
    impl Tmp {
        fn new(tag: &str) -> Self {
            let p = std::env::temp_dir().join(format!("atropos-proj-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&p);
            fs::create_dir_all(&p).unwrap();
            Self(p)
        }
    }
    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn project_from_package_json_name() {
        let t = Tmp::new("pkg");
        fs::write(
            t.0.join("package.json"),
            r#"{"name":"@acme/shop","version":"1.0.0"}"#,
        )
        .unwrap();
        fs::write(t.0.join("Cargo.toml"), "[package]\nname = \"other\"\n").unwrap();
        assert_eq!(project_name(&t.0).as_deref(), Some("@acme/shop"));
    }

    #[test]
    fn project_from_cargo_toml_name() {
        let t = Tmp::new("cargo");
        fs::write(
            t.0.join("Cargo.toml"),
            "[package]\nname = \"atropos\"\nversion = \"0.1.0\"\n",
        )
        .unwrap();
        assert_eq!(project_name(&t.0).as_deref(), Some("atropos"));
    }

    #[test]
    fn broken_or_nameless_manifests_fall_back_to_the_folder_name() {
        let t = Tmp::new("broken");
        let dir = t.0.join("My Projekt é");
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("package.json"), "{ not json").unwrap();
        fs::write(dir.join("Cargo.toml"), "[workspace]\nmembers = []\n").unwrap();
        assert_eq!(project_name(&dir).as_deref(), Some("My Projekt é"));
        // A trailing separator (as the PEB reports the cwd) changes nothing.
        let with_sep = PathBuf::from(format!("{}\\", dir.display()));
        assert_eq!(project_name(&with_sep).as_deref(), Some("My Projekt é"));
    }

    #[test]
    fn repo_root_is_the_nearest_git_ancestor() {
        let t = Tmp::new("repo");
        let repo = t.0.join("outer");
        let deep = repo.join("packages").join("web").join("src");
        fs::create_dir_all(&deep).unwrap();
        fs::create_dir_all(repo.join(".git")).unwrap();
        assert_eq!(repo_root(&deep), Some(repo.clone()));
        assert_eq!(repo_root(&repo), Some(repo.clone()));
        let with_sep = PathBuf::from(format!("{}\\", deep.display()));
        assert_eq!(repo_root(&with_sep), Some(repo.clone()));
        // A nested worktree/submodule has a `.git` FILE and wins over the outer repo.
        let sub = repo.join("packages").join("web");
        fs::write(sub.join(".git"), "gitdir: ../../.git/modules/web").unwrap();
        assert_eq!(repo_root(&deep), Some(sub));
    }

    #[test]
    fn probe_reports_markers_project_and_repo() {
        let t = Tmp::new("probe");
        fs::create_dir_all(t.0.join(".git")).unwrap();
        fs::write(t.0.join("package.json"), r#"{"name":"shop"}"#).unwrap();
        fs::write(t.0.join("App.SLN"), "").unwrap();
        let markers = vec![
            "package.json".to_owned(),
            "manage.py".to_owned(),
            "*.sln".to_owned(),
        ];
        let info = probe(&t.0, &markers);
        assert_eq!(
            info.markers,
            vec!["package.json".to_owned(), "*.sln".to_owned()]
        );
        assert_eq!(info.project.as_deref(), Some("shop"));
        assert_eq!(info.repo.as_deref(), t.0.to_str());
    }

    #[test]
    fn missing_directory_yields_only_the_folder_name() {
        let info = probe(
            Path::new(r"Z:\definitely\missing\thing\"),
            &["package.json".to_owned()],
        );
        assert_eq!(info.markers, Vec::<String>::new());
        assert_eq!(info.project.as_deref(), Some("thing"));
    }

    #[test]
    fn folder_name_handles_roots_and_unc() {
        assert_eq!(folder_name(r"D:\code\shop\"), "shop");
        assert_eq!(folder_name(r"D:\code\shop"), "shop");
        assert_eq!(folder_name(r"\\server\share\team\api\"), "api");
        assert_eq!(folder_name(r"D:\"), "D:");
    }
}
