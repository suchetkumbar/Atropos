//! The conformance suite (spec Phase 7) against the real backend of this OS. Every process the
//! scenarios act on was started here (`sleeper`, `forker`, `port-listener`, `file-locker`) and is
//! killed through its own handle when the backend is dropped.
#![cfg(any(windows, target_os = "linux", target_os = "macos"))]

use std::path::Path;
use std::time::Duration;

use atropos_core::ProcKey;
use atropos_core::platform::Platform;
use atropos_core::platform::conformance::{Backend, run_all};
#[cfg(target_os = "linux")]
use atropos_core::platform::linux::LinuxPlatform as Native;
#[cfg(target_os = "macos")]
use atropos_core::platform::macos::MacPlatform as Native;
#[cfg(windows)]
use atropos_core::platform::windows::WindowsPlatform as Native;
use atropos_fixtures::SpawnedFixture;

struct Real {
    p: Native,
    kids: Vec<SpawnedFixture>,
}

impl Real {
    fn keep(&mut self, f: SpawnedFixture) -> ProcKey {
        let key = f.key;
        self.kids.push(f);
        key
    }

    fn start(name: &str, args: &[&str]) -> Result<SpawnedFixture, String> {
        atropos_fixtures::spawn(name, args).map_err(|e| format!("spawn {name}: {e}"))
    }
}

impl Backend for Real {
    fn platform(&mut self) -> &mut dyn Platform {
        &mut self.p
    }

    fn spawn(&mut self, args: &[&str]) -> Result<ProcKey, String> {
        let mut all = vec!["--secs", "60"];
        all.extend_from_slice(args);
        let f = Self::start("sleeper", &all)?;
        Ok(self.keep(f))
    }

    fn spawn_tree(&mut self) -> Result<ProcKey, String> {
        let f = Self::start("forker", &["--max", "2", "--secs", "60"])?;
        Ok(self.keep(f))
    }

    fn spawn_listener(&mut self) -> Result<(ProcKey, u16), String> {
        let f = Self::start("port-listener", &["--secs", "60"])?;
        let port = f
            .next_line(Duration::from_secs(5))
            .and_then(|l| l.trim().strip_prefix("port=").and_then(|p| p.parse().ok()))
            .ok_or("port-listener printed no port")?;
        Ok((self.keep(f), port))
    }

    fn spawn_locker(&mut self, path: &Path) -> Result<ProcKey, String> {
        let p = path.to_str().ok_or("path is not UTF-8")?;
        let f = Self::start("file-locker", &["--path", p, "--secs", "60"])?;
        if f.next_line(Duration::from_secs(5))
            .as_deref()
            .map(str::trim)
            != Some("locked")
        {
            return Err("file-locker did not report the lock".into());
        }
        Ok(self.keep(f))
    }
}

#[test]
fn the_native_backend_passes_the_conformance_suite() {
    let mut b = Real {
        p: Native::new(),
        kids: Vec::new(),
    };
    let failures = run_all(&mut b);
    assert!(failures.is_empty(), "{failures:#?}");
}
