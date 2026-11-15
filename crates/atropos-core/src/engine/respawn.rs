//! Respawn warnings (spec Phase 4): ending a process is pointless when something will start it
//! again. This names that something, so the dialog can offer the right action instead.

use std::sync::Arc;

use crate::ProcKey;
use crate::labels::image_stem;
use crate::ports::ServiceInfo;
use crate::snapshot::Snapshot;
use crate::statics::StaticsMap;

/// How far up the parent chain a supervisor is looked for (`nodemon` → `cmd /c` → `node`).
const MAX_DEPTH: usize = 4;

/// Why the target would come back, and what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Respawn {
    /// The target hosts Windows services; the Service Control Manager may restart it.
    /// Right action: stop the service.
    Service { names: Vec<Arc<str>> },
    /// A watcher above the target restarts it. Right action: end the supervisor and its tree.
    Supervisor { key: ProcKey, name: &'static str },
    /// Started by Task Scheduler (`taskhostw`); it may run again on its schedule.
    ScheduledTask,
    /// Run by Docker Desktop. Right action: stop the container.
    Docker,
}

impl Respawn {
    /// One sentence for the confirmation dialog.
    #[must_use]
    pub fn warning(&self) -> String {
        match self {
            Respawn::Service { names } => format!(
                "This process runs the Windows service {}. Windows may start it again; stop the service instead of ending the process.",
                names
                    .iter()
                    .map(|n| format!("\"{n}\""))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Respawn::Supervisor { key, name } => format!(
                "{name} (PID {}) restarts this process when it exits. End {name} and its tree instead.",
                key.pid
            ),
            Respawn::ScheduledTask => "Started by Task Scheduler (taskhostw); it may start again on its schedule. Disable the task in Task Scheduler to stop it for good.".to_owned(),
            Respawn::Docker => "Managed by Docker Desktop, which starts it again. Stop the container instead (docker stop <container>).".to_owned(),
        }
    }
}

/// The supervisor a process is, by image and command line.
fn supervisor(image: &str, cmdline: Option<&str>) -> Option<&'static str> {
    let stem = image_stem(image);
    let cmd = cmdline.map(str::to_lowercase).unwrap_or_default();
    let has = |needle: &str| cmd.contains(needle);
    match stem.as_str() {
        "nodemon" => Some("nodemon"),
        "pm2" => Some("pm2"),
        "cargo-watch" => Some("cargo watch"),
        "dotnet-watch" => Some("dotnet watch"),
        "node" if has("nodemon") => Some("nodemon"),
        "node" if has("\\pm2\\") || has("/pm2/") || has("pm2 ") => Some("pm2"),
        "dotnet" if has(" watch") || has("dotnet-watch") => Some("dotnet watch"),
        "cargo" if has(" watch") => Some("cargo watch"),
        _ => None,
    }
}

fn is_docker(stem: &str) -> bool {
    matches!(
        stem,
        "com.docker.backend" | "docker desktop" | "dockerd" | "docker-proxy" | "com.docker.service"
    )
}

/// Whether `target` would be restarted after being ended, and by what.
#[must_use]
pub fn check(
    snap: &Snapshot,
    statics: &StaticsMap,
    services: &[ServiceInfo],
    target: ProcKey,
) -> Option<Respawn> {
    let me = snap.get(target)?;
    // A service is restarted by the Service Control Manager, whatever its parent is.
    let names: Vec<Arc<str>> = services
        .iter()
        .filter(|s| s.pid == target.pid)
        .map(|s| Arc::clone(&s.name))
        .collect();
    if !names.is_empty() {
        return Some(Respawn::Service { names });
    }
    if is_docker(&image_stem(&me.raw.image)) && image_stem(&me.raw.image) != "docker desktop" {
        return Some(Respawn::Docker);
    }
    let mut up = me.parent;
    for _ in 0..MAX_DEPTH {
        let p = &snap.procs[up?];
        let stem = image_stem(&p.raw.image);
        let cmdline = statics
            .get(&p.raw.key)
            .and_then(|s| s.cmdline.ok())
            .map(|c| &**c);
        if let Some(name) = supervisor(&p.raw.image, cmdline) {
            return Some(Respawn::Supervisor {
                key: p.raw.key,
                name,
            });
        }
        if stem == "taskhostw" {
            return Some(Respawn::ScheduledTask);
        }
        if is_docker(&stem) {
            return Some(Respawn::Docker);
        }
        up = p.parent;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Field;
    use crate::platform::{RawProc, SystemStats};
    use crate::snapshot::build;
    use crate::statics::Statics;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    /// `(pid, ppid, image, cmdline)`.
    fn world(procs: &[(u32, u32, &str, &str)]) -> (Snapshot, StaticsMap) {
        let snap = build(
            None,
            procs
                .iter()
                .map(|(pid, ppid, image, _)| RawProc::new(k(*pid), *ppid, image))
                .collect(),
            SystemStats::default(),
        );
        let statics = procs
            .iter()
            .map(|(pid, _, _, cmdline)| {
                (
                    k(*pid),
                    Arc::new(Statics {
                        cmdline: Field::Ok(Arc::from(*cmdline)),
                        ..Statics::unavailable(&Field::Unsupported)
                    }),
                )
            })
            .collect();
        (snap, statics)
    }

    fn service(pid: u32, name: &str) -> ServiceInfo {
        ServiceInfo {
            pid,
            name: Arc::from(name),
            display: Arc::from(name),
        }
    }

    /// Test name, processes, expected warning for PID 50.
    type MatrixRow = (
        &'static str,
        Vec<(u32, u32, &'static str, &'static str)>,
        Option<Respawn>,
    );

    #[test]
    #[allow(clippy::too_many_lines)] // one table
    fn warning_matrix() {
        let rows: Vec<MatrixRow> = vec![
            (
                "nodemon through cmd /c",
                vec![
                    (
                        10,
                        0,
                        "node.exe",
                        r"node D:\shop\node_modules\nodemon\bin\nodemon.js server.js",
                    ),
                    (11, 10, "cmd.exe", "cmd.exe /d /s /c node server.js"),
                    (50, 11, "node.exe", "node server.js"),
                ],
                Some(Respawn::Supervisor {
                    key: k(10),
                    name: "nodemon",
                }),
            ),
            (
                "pm2 daemon",
                vec![
                    (
                        10,
                        0,
                        "node.exe",
                        r"node C:\Users\x\AppData\Roaming\npm\node_modules\pm2\lib\Daemon.js",
                    ),
                    (50, 10, "node.exe", "node api.js"),
                ],
                Some(Respawn::Supervisor {
                    key: k(10),
                    name: "pm2",
                }),
            ),
            (
                "dotnet watch",
                vec![
                    (10, 0, "dotnet.exe", "dotnet watch run"),
                    (50, 10, "Api.exe", "Api.exe"),
                ],
                Some(Respawn::Supervisor {
                    key: k(10),
                    name: "dotnet watch",
                }),
            ),
            (
                "cargo watch",
                vec![
                    (10, 0, "cargo-watch.exe", "cargo-watch.exe watch -x run"),
                    (11, 10, "cargo.exe", "cargo run"),
                    (50, 11, "app.exe", "target\\debug\\app.exe"),
                ],
                Some(Respawn::Supervisor {
                    key: k(10),
                    name: "cargo watch",
                }),
            ),
            (
                "scheduled task",
                vec![
                    (10, 0, "taskhostw.exe", "taskhostw.exe"),
                    (50, 10, "backup.exe", "backup.exe"),
                ],
                Some(Respawn::ScheduledTask),
            ),
            (
                "docker child",
                vec![
                    (10, 0, "com.docker.backend.exe", "com.docker.backend.exe"),
                    (50, 10, "com.docker.build.exe", "com.docker.build.exe"),
                ],
                Some(Respawn::Docker),
            ),
            (
                "docker port forward itself",
                vec![(50, 0, "docker-proxy.exe", "docker-proxy -proto tcp")],
                Some(Respawn::Docker),
            ),
            (
                "plain dev server from a shell",
                vec![
                    (10, 0, "pwsh.exe", "pwsh.exe"),
                    (50, 10, "node.exe", "node server.js"),
                ],
                None,
            ),
            (
                "the supervisor itself is not warned about",
                vec![(
                    50,
                    0,
                    "node.exe",
                    r"node D:\shop\node_modules\nodemon\bin\nodemon.js server.js",
                )],
                None,
            ),
        ];
        for (name, procs, want) in rows {
            let (snap, statics) = world(&procs);
            assert_eq!(check(&snap, &statics, &[], k(50)), want, "{name}");
        }
    }

    #[test]
    fn a_service_host_offers_stop_service_never_its_supervisor() {
        // services.exe is the parent of every service: it must never be named as the thing to end.
        let (snap, statics) = world(&[
            (10, 0, "services.exe", ""),
            (50, 10, "postgres.exe", "postgres.exe -D data"),
        ]);
        let services = [service(50, "postgresql-x64-16"), service(77, "Other")];
        assert_eq!(
            check(&snap, &statics, &services, k(50)),
            Some(Respawn::Service {
                names: vec![Arc::from("postgresql-x64-16")]
            })
        );
    }

    #[test]
    fn supervisor_search_stops_after_a_few_levels_and_at_unknown_targets() {
        let mut procs = vec![(1, 0, "dotnet.exe", "dotnet watch")];
        for pid in 2..=8 {
            procs.push((pid, pid - 1, "cmd.exe", "cmd.exe"));
        }
        let (snap, statics) = world(&procs);
        assert!(check(&snap, &statics, &[], k(3)).is_some());
        assert_eq!(check(&snap, &statics, &[], k(8)), None);
        assert_eq!(check(&snap, &statics, &[], k(4242)), None);
    }

    #[test]
    fn every_warning_names_the_alternative() {
        let s = Respawn::Supervisor {
            key: k(10),
            name: "nodemon",
        }
        .warning();
        assert!(s.contains("nodemon") && s.contains("PID 10"));
        assert!(
            Respawn::Service {
                names: vec![Arc::from("Spooler")]
            }
            .warning()
            .contains("\"Spooler\"")
        );
        assert!(Respawn::Docker.warning().contains("container"));
        assert!(Respawn::ScheduledTask.warning().contains("Task Scheduler"));
    }
}
