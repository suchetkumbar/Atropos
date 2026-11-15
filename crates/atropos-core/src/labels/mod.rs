//! Smart labels (spec §4.8): readable identities such as "Next.js dev · shop" from data rules.
//!
//! The built-in rules are embedded (`rules.toml`), so labels work with no config file; user rules
//! from `config.toml` run first. Matching is pure: the caller supplies what it knows about the
//! working directory ([`Dir`]) and is told when a rule needs it ([`Resolution::NeedDir`]).

use regex_lite::Regex;
use serde::{Deserialize, Serialize};

pub mod project;

pub use project::DirInfo;

const BUILTIN: &str = include_str!("rules.toml");
/// Command lines are matched up to this many bytes (hostile or huge command lines stay cheap).
pub const MAX_CMDLINE: usize = 8 * 1024;
/// Segments of a label template are joined with this.
const SEP: &str = " · ";

/// One rule as written in `rules.toml` or under `[[labels]]` in `config.toml`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct RuleDef {
    /// Image names without `.exe`, case-insensitive. Empty = any image.
    pub image: Vec<String>,
    /// Regex searched in the command line.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub cmdline: Option<String>,
    /// Files that must exist in the working directory (`*.sln` = any file with that suffix).
    pub markers: Vec<String>,
    pub label: String,
}

#[derive(Debug, Deserialize)]
struct RuleFile {
    rule: Vec<RuleDef>,
}

#[derive(Debug)]
struct Rule {
    image: Vec<String>,
    cmdline: Option<Regex>,
    markers: Vec<String>,
    label: String,
    needs_dir: bool,
}

/// What the caller knows about the process's working directory.
#[derive(Debug, Clone, Copy)]
pub enum Dir<'a> {
    /// Not looked at yet.
    Unknown,
    /// Cannot be read (restricted, gone): marker rules do not match and `{project}` is empty.
    Unavailable,
    Known(&'a DirInfo),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Resolution {
    /// Final answer; `None` = no rule matched (show the image name).
    Label(Option<String>),
    /// The first candidate rule needs the working directory: call again with it.
    NeedDir,
}

/// The compiled rule set.
#[derive(Debug)]
pub struct Labels {
    rules: Vec<Rule>,
}

/// Lower-cases an image name and removes a trailing `.exe`.
#[must_use]
pub fn image_stem(image: &str) -> String {
    let lower = image.to_lowercase();
    match lower.strip_suffix(".exe") {
        Some(stem) => stem.to_owned(),
        None => lower,
    }
}

fn compile(def: &RuleDef) -> Result<Rule, String> {
    if def.label.trim().is_empty() {
        return Err("label is empty".to_owned());
    }
    if def.image.is_empty() && def.cmdline.is_none() && def.markers.is_empty() {
        return Err(format!(
            "rule \"{}\" has no image, cmdline or markers",
            def.label
        ));
    }
    let cmdline = match &def.cmdline {
        Some(src) => Some(
            Regex::new(src)
                .map_err(|e| format!("rule \"{}\": bad cmdline regex: {e}", def.label))?,
        ),
        None => None,
    };
    Ok(Rule {
        image: def.image.iter().map(|i| image_stem(i)).collect(),
        cmdline,
        markers: def.markers.iter().map(|m| m.to_lowercase()).collect(),
        needs_dir: !def.markers.is_empty() || def.label.contains("{project}"),
        label: def.label.clone(),
    })
}

/// Cuts `s` to at most `max` bytes on a character boundary.
fn cap(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Fills `template`; a segment whose placeholders are all empty is dropped.
fn render(template: &str, value: &dyn Fn(&str) -> Option<String>) -> Option<String> {
    let mut out: Vec<String> = Vec::new();
    for seg in template.split(SEP) {
        let mut text = String::new();
        let (mut holes, mut filled) = (0, 0);
        let mut rest = seg;
        while let Some(open) = rest.find('{') {
            let Some(len) = rest[open..].find('}') else {
                break;
            };
            text.push_str(&rest[..open]);
            holes += 1;
            if let Some(v) = value(&rest[open + 1..open + len]).filter(|v| !v.trim().is_empty()) {
                filled += 1;
                text.push_str(v.trim());
            }
            rest = &rest[open + len + 1..];
        }
        text.push_str(rest);
        let text = text.trim();
        if (holes == 0 || filled > 0) && !text.is_empty() {
            out.push(text.to_owned());
        }
    }
    (!out.is_empty()).then(|| out.join(SEP))
}

/// Problems in user rules, one line each (the same text [`Labels::with_user`] reports), without
/// compiling the built-ins. Cheap enough for the UI thread.
#[must_use]
pub fn check_user(user: &[RuleDef]) -> Vec<String> {
    user.iter()
        .enumerate()
        .filter_map(|(i, def)| {
            compile(def)
                .err()
                .map(|e| format!("label rule {} skipped: {e}", i + 1))
        })
        .collect()
}

impl Labels {
    /// The embedded rule set.
    #[must_use]
    pub fn builtin() -> Self {
        Self::with_user(&[]).0
    }

    /// User rules first, then the built-ins. A broken user rule is skipped and reported; it never
    /// disables the others (Z3).
    #[must_use]
    pub fn with_user(user: &[RuleDef]) -> (Self, Vec<String>) {
        let mut warnings = Vec::new();
        let mut rules = Vec::new();
        for (i, def) in user.iter().enumerate() {
            match compile(def) {
                Ok(r) => rules.push(r),
                Err(e) => warnings.push(format!("label rule {} skipped: {e}", i + 1)),
            }
        }
        match toml::from_str::<RuleFile>(BUILTIN) {
            Ok(file) => {
                for def in &file.rule {
                    match compile(def) {
                        Ok(r) => rules.push(r),
                        Err(e) => tracing::error!("built-in label rule: {e}"),
                    }
                }
            }
            Err(e) => tracing::error!("built-in label rules: {e}"),
        }
        (Self { rules }, warnings)
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.rules.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rules.is_empty()
    }

    /// Every marker any rule asks for (lower-case), for the directory prober.
    #[must_use]
    pub fn markers(&self) -> Vec<String> {
        let mut all: Vec<String> = self
            .rules
            .iter()
            .flat_map(|r| r.markers.iter().cloned())
            .collect();
        all.sort();
        all.dedup();
        all
    }

    /// Labels a process. `cmdline` is `None` when it cannot be read (cmdline rules then never
    /// match).
    #[must_use]
    pub fn resolve(&self, image: &str, cmdline: Option<&str>, dir: Dir<'_>) -> Resolution {
        let stem = image_stem(image);
        let cmdline = cmdline.map(|c| cap(c, MAX_CMDLINE));
        for rule in &self.rules {
            if !rule.image.is_empty() && !rule.image.contains(&stem) {
                continue;
            }
            let caps = match (&rule.cmdline, cmdline) {
                (None, _) => None,
                (Some(_), None) => continue,
                (Some(re), Some(c)) => match re.captures(c) {
                    Some(caps) => Some(caps),
                    None => continue,
                },
            };
            let info = match dir {
                Dir::Known(info) => Some(info),
                Dir::Unknown if rule.needs_dir => return Resolution::NeedDir,
                Dir::Unavailable | Dir::Unknown => None,
            };
            if !rule.markers.is_empty()
                && !info.is_some_and(|i| rule.markers.iter().all(|m| i.markers.contains(m)))
            {
                continue;
            }
            let label = render(&rule.label, &|name| {
                if name == "project" {
                    return info.and_then(|i| i.project.as_deref().map(str::to_owned));
                }
                let named = rule
                    .cmdline
                    .as_ref()?
                    .capture_names()
                    .flatten()
                    .any(|n| n == name);
                if !named {
                    return None;
                }
                caps.as_ref()?.name(name).map(|m| m.as_str().to_owned())
            });
            if label.is_some() {
                return Resolution::Label(label);
            }
        }
        Resolution::Label(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn dir(project: &str, markers: &[&str]) -> DirInfo {
        DirInfo {
            markers: markers.iter().map(|m| (*m).to_owned()).collect(),
            project: Some(Arc::from(project)),
            repo: None,
        }
    }

    fn label(image: &str, cmdline: &str, d: &DirInfo) -> Option<String> {
        match Labels::builtin().resolve(image, Some(cmdline), Dir::Known(d)) {
            Resolution::Label(l) => l,
            Resolution::NeedDir => panic!("NeedDir with a known dir"),
        }
    }

    const NODE: &str = r#""C:\Program Files\nodejs\node.exe""#;
    const CODE: &str = r#""C:\Programs\Microsoft VS Code\Code.exe""#;
    const CHROME: &str = r#""C:\Program Files\Google\Chrome\Application\chrome.exe""#;
    const EDGE: &str = r#""C:\Program Files (x86)\Microsoft\Edge\Application\msedge.exe""#;

    /// One row per framework required by spec §4.8, with command lines as Windows records them.
    #[test]
    #[allow(clippy::too_many_lines)]
    fn required_frameworks_are_labelled() {
        let shop = dir("shop", &["package.json"]);
        let py = dir("blog", &["manage.py"]);
        let rs = dir("atropos", &["cargo.toml"]);
        let none = dir("work", &[]);
        let nm = r"D:\code\shop\node_modules";
        let rows: Vec<(&str, String, &DirInfo, &str)> = vec![
            ("node.exe", format!(r"{NODE} {nm}\next\dist\bin\next dev"), &shop, "Next.js dev · shop"),
            ("node.exe", format!(r"{NODE} {nm}\next\dist\server\lib\start-server.js"), &shop, "Next.js dev · shop"),
            ("node.exe", format!(r"{NODE} {nm}\next\dist\bin\next start"), &shop, "Next.js · shop"),
            ("node.exe", format!(r"{NODE} {nm}\vite\bin\vite.js --host"), &shop, "Vite dev · shop"),
            ("node.exe", format!(r"{NODE} {nm}\react-scripts\scripts\start.js"), &shop, "Create React App · shop"),
            ("node.exe", format!(r"{NODE} {nm}\@angular\cli\bin\ng.js serve"), &shop, "Angular dev · shop"),
            ("node.exe", format!(r"{NODE} {nm}\nuxt\bin\nuxt.mjs dev"), &shop, "Nuxt dev · shop"),
            ("node.exe", format!(r"{NODE} {nm}\nodemon\bin\nodemon.js server.js"), &shop, "nodemon · shop"),
            ("node.exe", "node server.js".to_owned(), &shop, "Node app · shop"),
            ("node.exe", format!(r"{NODE} {nm}\ts-node\dist\bin.js src\index.ts"), &shop, "ts-node · shop"),
            ("node.exe", format!(r"{NODE} {nm}\tsx\dist\cli.mjs watch src/index.ts"), &shop, "tsx · shop"),
            ("python.exe", "python manage.py runserver 0.0.0.0:8000".to_owned(), &py, "Django dev · blog"),
            ("python.exe", "python -m flask run --debug".to_owned(), &py, "Flask dev · blog"),
            ("flask.exe", r"C:\venv\Scripts\flask.exe run".to_owned(), &py, "Flask dev · blog"),
            ("python.exe", "python -m uvicorn main:app --reload".to_owned(), &py, "Uvicorn · blog"),
            ("uvicorn.exe", r"C:\venv\Scripts\uvicorn.exe main:app".to_owned(), &py, "Uvicorn · blog"),
            ("fastapi.exe", r"C:\venv\Scripts\fastapi.exe dev main.py".to_owned(), &py, "FastAPI dev · blog"),
            ("python.exe", r"C:\venv\Scripts\python.exe C:\venv\Scripts\jupyter-lab.exe".to_owned(), &py, "Jupyter Lab · blog"),
            ("python.exe", r"python -m ipykernel_launcher -f C:\Users\x\kernel-1.json".to_owned(), &py, "Jupyter kernel"),
            ("cargo.exe", r"C:\Users\x\.cargo\bin\cargo.exe run --release".to_owned(), &rs, "cargo run · atropos"),
            ("cargo.exe", "cargo +nightly run".to_owned(), &rs, "cargo run · atropos"),
            ("cargo.exe", "cargo watch -x run".to_owned(), &rs, "cargo watch · atropos"),
            ("cargo-watch.exe", r"C:\Users\x\.cargo\bin\cargo-watch.exe watch -x run".to_owned(), &rs, "cargo watch · atropos"),
            ("dotnet.exe", r#""C:\Program Files\dotnet\dotnet.exe" run --project Api"#.to_owned(), &none, "dotnet run · work"),
            ("dotnet.exe", "dotnet watch run".to_owned(), &none, "dotnet watch · work"),
            ("java.exe", r"java -Dspring.profiles.active=dev -jar target\orders-1.2.jar".to_owned(), &none, "Spring Boot · orders-1.2"),
            ("java.exe", r#"java -jar "C:\tools\thing.jar""#.to_owned(), &none, "Java · thing"),
            ("java.exe", r"java -cp C:\g\lib\gradle-launcher-8.5.jar org.gradle.launcher.daemon.bootstrap.GradleDaemon 8.5".to_owned(), &none, "Gradle daemon · 8.5"),
            ("Docker Desktop.exe", r#""C:\Program Files\Docker\Docker\Docker Desktop.exe""#.to_owned(), &none, "Docker Desktop"),
            ("com.docker.backend.exe", "com.docker.backend.exe -addr unix://".to_owned(), &none, "Docker Desktop backend"),
            ("wslhost.exe", r"C:\Program Files\WSL\wslhost.exe --vm-id {x}".to_owned(), &none, "WSL host"),
            ("vmmem", String::new(), &none, "WSL / Hyper-V virtual machine"),
            ("vmmemWSL", String::new(), &none, "WSL / Hyper-V virtual machine"),
            ("Code.exe", format!(r#"{CODE} --type=renderer --user-data-dir="C:\Users\x\AppData\Roaming\Code""#), &none, "VS Code renderer"),
            ("Code.exe", format!("{CODE} --type=utility --utility-sub-type=node.mojom.NodeService --lang=en-US"), &none, "VS Code extension host"),
            ("Code.exe", CODE.to_owned(), &none, "VS Code"),
            ("rust-analyzer.exe", r"c:\Users\x\.vscode\extensions\rust-lang.rust-analyzer\server\rust-analyzer.exe".to_owned(), &rs, "rust-analyzer"),
            ("Code.exe", format!(r#"{CODE} "c:\Programs\Microsoft VS Code\resources\app\extensions\node_modules\typescript\lib\tsserver.js" --useInferredProjectPerProjectRoot"#), &shop, "TypeScript server (tsserver)"),
            ("node.exe", format!(r"{NODE} {nm}\typescript\lib\tsserver.js"), &shop, "TypeScript server (tsserver)"),
            ("Code.exe", format!(r"{CODE} c:\Users\x\.vscode\extensions\ms-python.vscode-pylance-2026.1.1\dist\server.bundle.js --cancellationReceive=file:1"), &none, "Pylance"),
            ("chrome.exe", format!("{CHROME} --type=renderer --lang=en-US --renderer-client-id=7"), &none, "Chrome renderer"),
            ("chrome.exe", format!(r#"{CHROME} --type=renderer --user-data-dir="C:\Users\x\AppData\Local\Google\Chrome\Work Profile" --lang=en-US"#), &none, "Chrome renderer · Work Profile"),
            ("chrome.exe", format!(r#"{CHROME} --profile-directory="Profile 2""#), &none, "Chrome · Profile 2"),
            ("chrome.exe", format!("{CHROME} --type=gpu-process --gpu-preferences=x"), &none, "Chrome gpu-process"),
            ("msedge.exe", format!(r"{EDGE} --type=renderer --user-data-dir=C:\e\Dev --lang=en"), &none, "Edge renderer · Dev"),
            ("msedge.exe", format!("{EDGE} --profile-directory=Default"), &none, "Edge · Default"),
        ];
        for (image, cmdline, d, want) in rows {
            assert_eq!(
                label(image, &cmdline, d).as_deref(),
                Some(want),
                "{image}: {cmdline}"
            );
        }
    }

    #[test]
    fn every_builtin_rule_compiles() {
        let file: RuleFile = toml::from_str(BUILTIN).unwrap();
        for def in &file.rule {
            compile(def).unwrap();
        }
        assert_eq!(Labels::builtin().len(), file.rule.len());
    }

    #[test]
    fn unmatched_processes_have_no_label() {
        let l = Labels::builtin();
        assert_eq!(
            l.resolve("svchost.exe", Some("svchost.exe -k netsvcs"), Dir::Unknown),
            Resolution::Label(None)
        );
        assert_eq!(
            l.resolve("chrome.exe", Some("chrome.exe"), Dir::Unknown),
            Resolution::Label(None)
        );
    }

    #[test]
    fn a_rule_that_needs_the_cwd_asks_for_it_once_image_and_cmdline_match() {
        let l = Labels::builtin();
        let next = r"node D:\s\node_modules\next\dist\bin\next dev";
        assert_eq!(
            l.resolve("node.exe", Some(next), Dir::Unknown),
            Resolution::NeedDir
        );
        // Rules without markers or {project} never ask.
        assert_eq!(
            l.resolve("wslhost.exe", Some("wslhost.exe"), Dir::Unknown),
            Resolution::Label(Some("WSL host".to_owned()))
        );
    }

    #[test]
    fn unreadable_cwd_labels_from_image_and_cmdline_only() {
        let l = Labels::builtin();
        let next = r"node D:\s\node_modules\next\dist\bin\next dev";
        assert_eq!(
            l.resolve("node.exe", Some(next), Dir::Unavailable),
            Resolution::Label(Some("Next.js dev".to_owned()))
        );
        // The marker rule ("Node app") cannot match without the directory.
        assert_eq!(
            l.resolve("node.exe", Some("node server.js"), Dir::Unavailable),
            Resolution::Label(None)
        );
    }

    #[test]
    fn unreadable_cmdline_matches_image_only_rules() {
        let l = Labels::builtin();
        assert_eq!(
            l.resolve("rust-analyzer.exe", None, Dir::Unknown),
            Resolution::Label(Some("rust-analyzer".to_owned()))
        );
        assert_eq!(
            l.resolve("node.exe", None, Dir::Unavailable),
            Resolution::Label(None)
        );
    }

    #[test]
    fn marker_rule_requires_the_marker() {
        let without = dir("tmp", &[]);
        assert_eq!(label("node.exe", "node server.js", &without), None);
    }

    #[test]
    fn user_rules_run_first_and_a_broken_one_is_reported_not_fatal() {
        let user = vec![
            RuleDef {
                image: vec!["node".into()],
                cmdline: Some("(unclosed".into()),
                label: "Broken".into(),
                ..RuleDef::default()
            },
            RuleDef {
                image: vec!["Node.EXE".into()],
                cmdline: Some(r"(?i)\bapi[\\/]server\.js".into()),
                label: "Orders API · {project}".into(),
                ..RuleDef::default()
            },
            RuleDef::default(),
        ];
        let (l, warnings) = Labels::with_user(&user);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert_eq!(check_user(&user), warnings);
        assert!(warnings[0].starts_with("label rule 1 skipped"));
        assert!(warnings[1].starts_with("label rule 3 skipped"));
        let d = dir("shop", &["package.json"]);
        assert_eq!(
            l.resolve("node.exe", Some(r"node api\server.js"), Dir::Known(&d)),
            Resolution::Label(Some("Orders API · shop".to_owned()))
        );
        // Built-ins still work.
        assert_eq!(
            l.resolve("wslhost.exe", None, Dir::Unknown),
            Resolution::Label(Some("WSL host".to_owned()))
        );
    }

    #[test]
    fn huge_and_odd_command_lines_are_bounded() {
        let l = Labels::builtin();
        let mut huge = String::from("node ");
        huge.push_str(&"é\u{202e}\\next\\dist\\ ".repeat(100_000)); // ~1.5 MB
        huge.push_str(r"\node_modules\next\dist\bin\next dev");
        let started = std::time::Instant::now();
        let d = dir("p", &["package.json"]);
        // The match lies beyond the cap, so only the marker rule can apply.
        assert_eq!(
            l.resolve("node.exe", Some(&huge), Dir::Known(&d)),
            Resolution::Label(Some("Node app · p".to_owned()))
        );
        assert!(started.elapsed() < std::time::Duration::from_millis(500));
    }

    #[test]
    fn cap_respects_char_boundaries() {
        assert_eq!(cap("aé", 2), "a");
        assert_eq!(cap("abc", 8), "abc");
    }

    #[test]
    fn markers_lists_every_marker_once() {
        assert!(
            Labels::builtin()
                .markers()
                .contains(&"package.json".to_owned())
        );
    }
}
