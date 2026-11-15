//! Snapshot export for bug reports (spec Phase 6): every process with its tree position,
//! statics, label / cwd / repository and sockets, as one JSON document. Secret-looking values
//! in command lines are masked unless the caller asks for the raw text (the GUI asks the user
//! first). Environment variables are never exported.

use serde::Serialize;

use crate::context::ContextMap;
use crate::ports::{Proto, Socket, TcpState};
use crate::secrets::mask_cmdline;
use crate::snapshot::Snapshot;
use crate::statics::StaticsMap;

pub const SCHEMA: u8 = 1;

#[derive(Debug, Serialize, PartialEq)]
pub struct Export {
    pub schema: u8,
    pub atropos: String,
    /// FILETIME ticks (100 ns since 1601).
    pub taken_at: u64,
    pub masked: bool,
    pub logical_cpus: u32,
    pub total_ram: u64,
    pub used_ram: u64,
    pub processes: Vec<ProcOut>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct ProcOut {
    pub pid: u32,
    pub create_time: u64,
    /// The validated parent (create time checked); `null` for a root.
    pub parent_pid: Option<u32>,
    pub name: String,
    pub label: Option<String>,
    pub exe: Option<String>,
    pub cmdline: Option<String>,
    pub user: Option<String>,
    pub cwd: Option<String>,
    pub repo: Option<String>,
    pub cpu_pct: Option<f32>,
    pub private_ws: u64,
    pub threads: u32,
    pub handles: u32,
    pub suspended: bool,
    pub sockets: Vec<SocketOut>,
}

#[derive(Debug, Serialize, PartialEq)]
pub struct SocketOut {
    pub proto: &'static str,
    pub local: String,
    pub remote: Option<String>,
    pub state: Option<&'static str>,
}

/// Builds the export. `sockets` is `None` when they were not read (then every list is empty).
#[must_use]
pub fn build(
    snap: &Snapshot,
    statics: &StaticsMap,
    context: &ContextMap,
    sockets: Option<&[Socket]>,
    masked: bool,
    taken_at: u64,
) -> Export {
    let text = |f: &crate::Field<std::sync::Arc<str>>| f.ok().map(ToString::to_string);
    let processes = snap
        .procs
        .iter()
        .map(|p| {
            let key = p.raw.key;
            let st = statics.get(&key);
            let ctx = context.get(&key);
            let cmdline = st
                .and_then(|s| text(&s.cmdline))
                .map(|c| if masked { mask_cmdline(&c) } else { c });
            ProcOut {
                pid: key.pid,
                create_time: key.create_time,
                parent_pid: p.parent.map(|i| snap.procs[i].raw.key.pid),
                name: p.raw.image.to_string(),
                label: ctx.and_then(|c| c.label.as_deref()).map(str::to_owned),
                exe: st.and_then(|s| text(&s.exe_path)),
                cmdline,
                user: st.and_then(|s| text(&s.user)),
                cwd: ctx.and_then(|c| c.cwd.as_deref()).map(str::to_owned),
                repo: ctx.and_then(|c| c.repo.as_deref()).map(str::to_owned),
                cpu_pct: p.cpu_pct,
                private_ws: p.raw.private_ws,
                threads: p.raw.threads,
                handles: p.raw.handles,
                suspended: p.raw.suspended,
                sockets: sockets
                    .unwrap_or_default()
                    .iter()
                    .filter(|s| s.owner == key.pid)
                    .map(|s| SocketOut {
                        proto: match s.proto {
                            Proto::Tcp => "tcp",
                            Proto::Udp => "udp",
                        },
                        local: s.local.to_string(),
                        remote: s.remote.map(|r| r.to_string()),
                        state: s.state.map(|st| match st {
                            TcpState::Listen => "listen",
                            TcpState::Established => "established",
                            TcpState::Other(_) => "other",
                        }),
                    })
                    .collect(),
            }
        })
        .collect();
    Export {
        schema: SCHEMA,
        atropos: env!("CARGO_PKG_VERSION").to_owned(),
        taken_at,
        masked,
        logical_cpus: snap.stats.logical_cpus,
        total_ram: snap.stats.total_ram,
        used_ram: snap.stats.used_ram,
        processes,
    }
}

/// Pretty JSON with a trailing newline.
#[must_use]
pub fn to_json(e: &Export) -> String {
    let mut s = serde_json::to_string_pretty(e).unwrap_or_default();
    s.push('\n');
    s
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::context::Ctx;
    use crate::platform::{RawProc, SystemStats};
    use crate::statics::Statics;
    use crate::{Field, ProcKey};
    use std::sync::Arc;

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid) * 100,
        }
    }

    /// explorer(1) → node(2, "Next.js dev · shop", listening on 3000, a token on its command
    /// line); lsass(9) unreadable.
    fn input() -> (Snapshot, StaticsMap, ContextMap, Vec<Socket>) {
        let stats = |now| SystemStats {
            logical_cpus: 4,
            total_ram: 16 << 30,
            used_ram: 8 << 30,
            now_100ns: now,
        };
        let procs = |cpu: u64| {
            let mut n = RawProc::new(k(2), 1, "node.exe");
            n.cpu_100ns = cpu;
            n.private_ws = 200 << 20;
            n.threads = 11;
            n.handles = 300;
            vec![
                RawProc::new(k(1), 0, "explorer.exe"),
                n,
                RawProc::new(k(9), 0, "lsass.exe"),
            ]
        };
        let first = crate::snapshot::build(None, procs(0), stats(0));
        let snap = crate::snapshot::build(Some(&first), procs(10_000_000), stats(40_000_000));
        let mut statics = StaticsMap::new();
        statics.insert(
            k(2),
            Arc::new(Statics {
                exe_path: Field::Ok(Arc::from(r"C:\Program Files\nodejs\node.exe")),
                cmdline: Field::Ok(Arc::from("node server.js --api-token=abc123")),
                user: Field::Ok(Arc::from("josé")),
                wow64: Field::Ok(false),
                protection: Field::Ok(crate::statics::Protection::None),
            }),
        );
        statics.insert(k(9), Arc::new(Statics::unavailable(&Field::Restricted)));
        let mut context = ContextMap::new();
        context.insert(
            k(2),
            Ctx {
                label: Some(Arc::from("Next.js dev · shop")),
                cwd: Some(Arc::from(r"D:\code\shop\")),
                repo: Some(Arc::from(r"D:\code\shop")),
            },
        );
        let sockets = vec![Socket {
            proto: Proto::Tcp,
            local: "[::1]:3000".parse().unwrap(),
            remote: None,
            state: Some(TcpState::Listen),
            owner: 2,
        }];
        (snap, statics, context, sockets)
    }

    fn golden() -> String {
        std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/export.json"),
        )
        .unwrap_or_default()
        .replace("\r\n", "\n")
    }

    #[test]
    fn export_matches_the_golden_file() {
        let (snap, statics, context, sockets) = input();
        let mut e = build(
            &snap,
            &statics,
            &context,
            Some(&sockets),
            true,
            133_000_000_000_000_000,
        );
        e.atropos = "test".into();
        let got = to_json(&e);
        if std::env::var_os("ATROPOS_UPDATE_GOLDEN").is_some() {
            std::fs::write(
                std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/golden/export.json"),
                &got,
            )
            .unwrap();
        }
        assert_eq!(got, golden());
    }

    #[test]
    fn unmasked_keeps_the_command_line_and_nothing_leaks_when_masked() {
        let (snap, statics, context, sockets) = input();
        let masked = to_json(&build(&snap, &statics, &context, Some(&sockets), true, 0));
        assert!(!masked.contains("abc123"));
        let raw = build(&snap, &statics, &context, Some(&sockets), false, 0);
        let node = raw.processes.iter().find(|p| p.pid == 2).unwrap();
        assert_eq!(
            node.cmdline.as_deref(),
            Some("node server.js --api-token=abc123")
        );
        assert!(!raw.masked);
    }
}
