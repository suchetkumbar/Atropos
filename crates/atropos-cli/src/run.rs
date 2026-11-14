//! The `atr` commands, written against `Platform` so they run on the fake backend in tests.
//! Exit codes (spec): `port` 0 found/done · 1 none · 2 denied · 3 stuck; `find` 0/1;
//! `kill` 0 · 2 denied/protected · 3 stuck (1 = nothing matched or cancelled); `tree` 0;
//! `wait-port` 0 free · 4 timeout. `--json` output always carries `"schema": 1`.

use std::io::Write;
use std::sync::Arc;
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::engine::kill::{KillOpts, Outcome, kill};
use atropos_core::engine::policy::{self, ActionKind, Context, HardReason, SoftReason, Verdict};
use atropos_core::engine::subtree::{console_roots, kill_tree, preorder};
use atropos_core::filter::{Filter, FilterMatcher, Subject};
use atropos_core::pin::Access;
use atropos_core::platform::Platform;
use atropos_core::ports::{Badge, Proto, TcpState, badge, owners};
use atropos_core::snapshot::{Snapshot, build};
use atropos_core::statics::StaticsMap;
use serde::Serialize;

/// Everything a command needs.
pub struct Env<'a> {
    pub platform: &'a mut dyn Platform,
    /// `atr` itself (protected, and never a match).
    pub me: ProcKey,
    pub out: &'a mut dyn Write,
    pub json: bool,
    /// Asks the user (interactive terminal only); `false` = no.
    pub confirm: &'a mut dyn FnMut(&str) -> bool,
}

/// Options shared by `kill` and `port --kill`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::struct_excessive_bools)] // independent CLI flags
pub struct KillArgs {
    pub tree: bool,
    /// End every process started from the target's terminal window (implies `tree`).
    pub console: bool,
    pub force: bool,
    pub grace: Duration,
    pub yes: bool,
    pub dry_run: bool,
}

impl Default for KillArgs {
    fn default() -> Self {
        Self {
            tree: false,
            console: false,
            force: false,
            grace: Duration::from_secs(5),
            yes: false,
            dry_run: false,
        }
    }
}

/// `500ms`, `5s`, `2m`, or a plain number of seconds.
///
/// # Errors
/// If the text is not one of those forms.
pub fn parse_duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let bad = || format!("not a duration: {s:?} (use e.g. 500ms, 5s, 2m)");
    let (num, unit) = match s.find(|c: char| !c.is_ascii_digit()) {
        Some(i) => s.split_at(i),
        None => (s, "s"),
    };
    let n: u64 = num.parse().map_err(|_| bad())?;
    match unit {
        "ms" => Ok(Duration::from_millis(n)),
        "s" => Ok(Duration::from_secs(n)),
        "m" => Ok(Duration::from_secs(n.saturating_mul(60))),
        _ => Err(bad()),
    }
}

const SCHEMA: u8 = 1;

fn scan(platform: &mut dyn Platform) -> Snapshot {
    let mut raw = Vec::new();
    match platform.scan(&mut raw) {
        Ok(stats) => build(None, raw, stats),
        Err(_) => Snapshot::default(),
    }
}

fn emit_json<T: Serialize>(out: &mut dyn Write, v: &T) {
    if let Ok(s) = serde_json::to_string(v) {
        let _ = writeln!(out, "{s}");
    }
}

fn name_of(snap: &Snapshot, key: ProcKey) -> String {
    snap.get(key)
        .map_or_else(|| "?".to_owned(), |p| p.raw.image.to_string())
}

fn outcome_str(o: Outcome) -> &'static str {
    match o {
        Outcome::Exited => "exited",
        Outcome::Terminated => "terminated",
        Outcome::Denied => "denied",
        Outcome::Protected => "protected",
        Outcome::Gone => "gone",
        Outcome::Stuck => "stuck",
    }
}

fn hard_note(r: &HardReason) -> String {
    match r {
        HardReason::SystemPid => "core Windows process".to_owned(),
        HardReason::Critical(n) => format!("{n} is critical to Windows"),
        HardReason::SelfOrAncestor => "atr itself or a process it was started from".to_owned(),
        HardReason::Protected => "protected by Windows".to_owned(),
    }
}

fn soft_note(r: &SoftReason) -> String {
    match r {
        SoftReason::Shell(n) => format!("{n} is part of the Windows desktop"),
        SoftReason::MultiServiceHost(n) => format!("hosts {n} services"),
        SoftReason::OtherSession(s) => format!("runs in another session ({s})"),
    }
}

#[derive(Serialize)]
struct OwnerOut {
    pid: u32,
    create_time: u64,
    name: String,
    proto: &'static str,
    local: String,
    state: Option<&'static str>,
    badge: Option<&'static str>,
}

#[derive(Serialize)]
struct ResultOut {
    pid: u32,
    create_time: u64,
    name: String,
    outcome: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    note: Option<String>,
}

#[derive(Serialize)]
struct ActionOut {
    dry_run: bool,
    tree: bool,
    results: Vec<ResultOut>,
}

#[derive(Serialize)]
struct PortOut {
    schema: u8,
    port: u16,
    owners: Vec<OwnerOut>,
    #[serde(skip_serializing_if = "Option::is_none")]
    action: Option<ActionOut>,
}

#[derive(Serialize)]
struct KillOut {
    schema: u8,
    #[serde(flatten)]
    action: ActionOut,
}

#[derive(Serialize)]
struct MatchOut {
    pid: u32,
    create_time: u64,
    name: String,
    user: Option<String>,
    cmdline: Option<String>,
}

#[derive(Serialize)]
struct FindOut {
    schema: u8,
    query: String,
    matches: Vec<MatchOut>,
}

#[derive(Serialize)]
struct NodeOut {
    pid: u32,
    create_time: u64,
    name: String,
    children: Vec<NodeOut>,
}

#[derive(Serialize)]
struct TreeOut {
    schema: u8,
    roots: Vec<NodeOut>,
}

#[derive(Serialize)]
struct WaitOut {
    schema: u8,
    port: u16,
    free: bool,
    waited_ms: u64,
}

/// Exit code for a set of outcomes: any stuck → 3, any denied/protected → 2, else 0.
fn exit_for(results: &[ResultOut]) -> u8 {
    if results.iter().any(|r| r.outcome == "stuck") {
        3
    } else if results
        .iter()
        .any(|r| matches!(r.outcome, "denied" | "protected" | "refused"))
    {
        2
    } else {
        0
    }
}

/// Policy verdicts for `targets` (statics only for them; services for the svchost rule).
fn verdicts(
    env: &mut Env<'_>,
    snap: &Snapshot,
    targets: &[ProcKey],
    kind: ActionKind,
) -> Vec<Verdict> {
    let services = env.platform.services().unwrap_or_default();
    let mut statics = StaticsMap::new();
    for k in targets.iter().filter(|k| snap.index.contains_key(k)) {
        statics.insert(*k, Arc::new(env.platform.statics(*k)));
    }
    let ctx = Context {
        snapshot: snap,
        statics: &statics,
        services: &services,
        me: env.me,
    };
    targets
        .iter()
        .map(|k| policy::check(&ctx, *k, kind))
        .collect()
}

/// A dry run: what would happen to each target. Identity is checked with a query-only pin, so
/// `PROCESS_TERMINATE` is never requested.
fn dry_run(
    env: &mut Env<'_>,
    snap: &Snapshot,
    targets: &[ProcKey],
    verdicts: &[Verdict],
    tree: bool,
) -> (Vec<ResultOut>, u8) {
    let results: Vec<ResultOut> = targets
        .iter()
        .zip(verdicts)
        .map(|(&key, v)| match v {
            Verdict::Hard(h) => result(snap, key, "refused", Some(hard_note(h))),
            _ if env.platform.pin(key, Access::QUERY).is_err() => result(snap, key, "gone", None),
            Verdict::Soft(r) => result(snap, key, "needs_confirmation", Some(soft_note(r))),
            Verdict::Allowed => result(snap, key, "would_end", None),
        })
        .collect();
    if !env.json {
        for r in &results {
            let what = match r.outcome {
                "would_end" => "Would end",
                "needs_confirmation" => "Would ask before ending",
                "refused" => "Would refuse to end",
                _ => "Already gone:",
            };
            let tree = if tree { " and its descendants" } else { "" };
            let note = r
                .note
                .as_deref()
                .map_or_else(String::new, |n| format!(" ({n})"));
            let _ = writeln!(env.out, "{what} {} ({}){tree}{note}", r.name, r.pid);
        }
    }
    let code = if results.iter().any(|r| r.outcome == "refused") {
        2
    } else {
        0
    };
    (results, code)
}

fn result(snap: &Snapshot, key: ProcKey, outcome: &'static str, note: Option<String>) -> ResultOut {
    ResultOut {
        pid: key.pid,
        create_time: key.create_time,
        name: name_of(snap, key),
        outcome,
        note,
    }
}

/// Asks before acting (unless `--yes`); soft-blocked targets show why.
fn confirmed(
    env: &mut Env<'_>,
    snap: &Snapshot,
    targets: &[ProcKey],
    verdicts: &[Verdict],
    tree: bool,
) -> bool {
    let list: Vec<String> = targets
        .iter()
        .zip(verdicts)
        .map(|(&k, v)| {
            let warn = match v {
                Verdict::Soft(r) => format!(" — {}", soft_note(r)),
                _ => String::new(),
            };
            format!("{} ({}){warn}", name_of(snap, k), k.pid)
        })
        .collect();
    let tree = if tree { " and descendants" } else { "" };
    let ok = (env.confirm)(&format!("End {}{tree}?", list.join(", ")));
    if !ok && !env.json {
        let _ = writeln!(env.out, "Cancelled.");
    }
    ok
}

/// Ends `targets` (or reports what would happen, for a dry run). Returns the results and the
/// exit code; `None` when the user declined.
fn act(
    env: &mut Env<'_>,
    snap: &Snapshot,
    targets: &[ProcKey],
    args: KillArgs,
) -> Option<(Vec<ResultOut>, u8)> {
    // Terminal-window scope: the console's processes minus the shell they were started from.
    let (targets, args): (Vec<ProcKey>, KillArgs) = if args.console {
        let mut v = Vec::new();
        for &t in targets {
            let pids = env
                .platform
                .console_processes(t)
                .unwrap_or_else(|_| vec![t.pid]);
            for r in console_roots(snap, t, &pids) {
                if r != env.me && !v.contains(&r) {
                    v.push(r);
                }
            }
        }
        (v, KillArgs { tree: true, ..args })
    } else {
        (targets.to_vec(), args)
    };
    let targets = targets.as_slice();
    let kind = if args.tree {
        ActionKind::KillTree
    } else {
        ActionKind::Kill
    };
    let verdicts = verdicts(env, snap, targets, kind);
    if args.dry_run {
        return Some(dry_run(env, snap, targets, &verdicts, args.tree));
    }
    if !args.yes && !confirmed(env, snap, targets, &verdicts, args.tree) {
        return None;
    }
    let opts = KillOpts {
        grace: args.grace,
        force: args.force,
    };
    let mut results = Vec::new();
    for (&key, v) in targets.iter().zip(&verdicts) {
        let hard = matches!(v, Verdict::Hard(_));
        if args.tree {
            // Every member of the subtree gets its own policy check.
            let members = snap
                .index
                .get(&key)
                .map_or_else(|| vec![key], |&i| preorder(snap, i));
            let member_verdicts = self::verdicts(env, snap, &members, kind);
            let blocked: std::collections::HashMap<ProcKey, bool> = members
                .iter()
                .zip(&member_verdicts)
                .map(|(k, v)| (*k, matches!(v, Verdict::Hard(_))))
                .collect();
            let is_blocked =
                |k: ProcKey| blocked.get(&k).copied().unwrap_or(false) || (hard && k == key);
            let report = kill_tree(env.platform, key, opts, &is_blocked, &mut |_| {});
            for (k, o) in report.members {
                results.push(result(snap, k, outcome_str(o), None));
            }
        } else {
            let o = kill(env.platform, key, opts, hard, &mut |_| {});
            let note = match v {
                Verdict::Hard(h) => Some(hard_note(h)),
                _ => None,
            };
            results.push(result(snap, key, outcome_str(o), note));
        }
    }
    if !env.json {
        for r in &results {
            let note = r
                .note
                .as_deref()
                .map_or_else(String::new, |n| format!(" ({n})"));
            let _ = writeln!(env.out, "{} ({}): {}{note}", r.name, r.pid, r.outcome);
        }
    }
    let code = exit_for(&results);
    Some((results, code))
}

pub fn port(env: &mut Env<'_>, port: u16, kill_args: Option<KillArgs>) -> u8 {
    let snap = scan(env.platform);
    let sockets = env.platform.sockets().unwrap_or_default();
    let found = owners(&snap, &sockets, port);
    let owners_out: Vec<OwnerOut> = found
        .iter()
        .map(|(k, s)| OwnerOut {
            pid: k.pid,
            create_time: k.create_time,
            name: name_of(&snap, *k),
            proto: match s.proto {
                Proto::Tcp => "tcp",
                Proto::Udp => "udp",
            },
            local: s.local.to_string(),
            state: s.state.map(|st| match st {
                TcpState::Listen => "listen",
                TcpState::Established => "established",
                TcpState::Other(_) => "other",
            }),
            badge: badge(&name_of(&snap, *k)).map(Badge::text),
        })
        .collect();
    if !env.json {
        if owners_out.is_empty() {
            let _ = writeln!(env.out, "Nothing is using port {port}");
        }
        for o in &owners_out {
            let state = o.state.map_or(String::new(), |s| format!(" {s}"));
            let badge = o.badge.map_or(String::new(), |b| format!("  [{b}]"));
            let _ = writeln!(
                env.out,
                "{port}  {} (PID {})  {} {}{state}{badge}",
                o.name,
                o.pid,
                o.proto.to_uppercase(),
                o.local
            );
        }
    }
    let mut code = u8::from(found.is_empty());
    let mut action = None;
    if let (Some(args), false) = (kill_args, found.is_empty()) {
        let mut targets: Vec<ProcKey> = Vec::new();
        for (k, _) in &found {
            if !targets.contains(k) {
                targets.push(*k);
            }
        }
        match act(env, &snap, &targets, args) {
            Some((results, c)) => {
                code = c;
                action = Some(ActionOut {
                    dry_run: args.dry_run,
                    tree: args.tree,
                    results,
                });
            }
            None => code = 1,
        }
    }
    if env.json {
        emit_json(
            env.out,
            &PortOut {
                schema: SCHEMA,
                port,
                owners: owners_out,
                action,
            },
        );
    }
    code
}

/// Processes matching `query` (filter syntax), never `atr` itself.
fn matches(env: &mut Env<'_>, snap: &Snapshot, query: &str) -> (Vec<ProcKey>, StaticsMap) {
    let filter = Filter::parse(query);
    let sockets = if filter.has_port_term() {
        env.platform.sockets().ok()
    } else {
        None
    };
    let mut statics = StaticsMap::new();
    let mut m = FilterMatcher::default();
    let me_user = env
        .platform
        .statics(env.me)
        .user
        .ok()
        .map(ToString::to_string)
        .unwrap_or_default();
    let mut keys = Vec::new();
    for p in snap
        .procs
        .iter()
        .filter(|p| p.raw.key != env.me && p.raw.key.pid != 0)
    {
        let st = Arc::new(env.platform.statics(p.raw.key));
        let ports: Option<Vec<u16>> = sockets.as_ref().map(|ss| {
            ss.iter()
                .filter(|s| s.owner == p.raw.key.pid)
                .map(|s| s.local.port())
                .collect()
        });
        let subject = Subject {
            pid: p.raw.key.pid,
            image: &p.raw.image,
            label: None,
            cmdline: st.cmdline.ok().map(|c| &**c),
            user: st.user.ok().map(|u| &**u),
            cwd: None,
            ports: ports.as_deref(),
        };
        if filter.matches(&subject, &me_user, &mut m).is_some() {
            keys.push(p.raw.key);
        }
        statics.insert(p.raw.key, st);
    }
    (keys, statics)
}

pub fn find(env: &mut Env<'_>, query: &str) -> u8 {
    let snap = scan(env.platform);
    let (keys, statics) = matches(env, &snap, query);
    let out: Vec<MatchOut> = keys
        .iter()
        .map(|k| {
            let st = statics.get(k);
            MatchOut {
                pid: k.pid,
                create_time: k.create_time,
                name: name_of(&snap, *k),
                user: st.and_then(|s| s.user.ok()).map(ToString::to_string),
                cmdline: st.and_then(|s| s.cmdline.ok()).map(ToString::to_string),
            }
        })
        .collect();
    let code = u8::from(out.is_empty());
    if env.json {
        emit_json(
            env.out,
            &FindOut {
                schema: SCHEMA,
                query: query.to_owned(),
                matches: out,
            },
        );
    } else if out.is_empty() {
        let _ = writeln!(env.out, "No process matches {query:?}");
    } else {
        for m in &out {
            let _ = writeln!(
                env.out,
                "{:>7}  {}  {}",
                m.pid,
                m.name,
                m.cmdline.as_deref().unwrap_or("")
            );
        }
    }
    code
}

pub fn kill_cmd(env: &mut Env<'_>, target: &str, args: KillArgs) -> u8 {
    let snap = scan(env.platform);
    let targets: Vec<ProcKey> = match target.parse::<u32>() {
        Ok(pid) => snap
            .procs
            .iter()
            .filter(|p| p.raw.key.pid == pid)
            .map(|p| p.raw.key)
            .collect(),
        Err(_) => matches(env, &snap, target).0,
    };
    if targets.is_empty() {
        if env.json {
            emit_json(
                env.out,
                &KillOut {
                    schema: SCHEMA,
                    action: ActionOut {
                        dry_run: args.dry_run,
                        tree: args.tree,
                        results: Vec::new(),
                    },
                },
            );
        } else {
            let _ = writeln!(env.out, "No process matches {target:?}");
        }
        return 1;
    }
    let Some((results, code)) = act(env, &snap, &targets, args) else {
        return 1;
    };
    if env.json {
        emit_json(
            env.out,
            &KillOut {
                schema: SCHEMA,
                action: ActionOut {
                    dry_run: args.dry_run,
                    tree: args.tree,
                    results,
                },
            },
        );
    }
    code
}

fn node(snap: &Snapshot, i: usize) -> NodeOut {
    let p = &snap.procs[i];
    NodeOut {
        pid: p.raw.key.pid,
        create_time: p.raw.key.create_time,
        name: p.raw.image.to_string(),
        children: snap.children[i].iter().map(|&c| node(snap, c)).collect(),
    }
}

fn print_node(out: &mut dyn Write, n: &NodeOut, depth: usize) {
    let _ = writeln!(out, "{}{} ({})", "  ".repeat(depth), n.name, n.pid);
    for c in &n.children {
        print_node(out, c, depth + 1);
    }
}

pub fn tree(env: &mut Env<'_>, pid: Option<u32>) -> u8 {
    let snap = scan(env.platform);
    let roots: Vec<usize> = match pid {
        Some(pid) => snap
            .procs
            .iter()
            .position(|p| p.raw.key.pid == pid)
            .into_iter()
            .collect(),
        None => snap.roots.clone(),
    };
    let nodes: Vec<NodeOut> = roots.iter().map(|&i| node(&snap, i)).collect();
    if env.json {
        emit_json(
            env.out,
            &TreeOut {
                schema: SCHEMA,
                roots: nodes,
            },
        );
    } else if nodes.is_empty() {
        let _ = writeln!(env.out, "No process with PID {}", pid.unwrap_or(0));
    } else {
        for n in &nodes {
            print_node(env.out, n, 0);
        }
    }
    0
}

pub fn wait_port(env: &mut Env<'_>, port: u16, timeout: Duration) -> u8 {
    let start = Instant::now();
    let free = loop {
        let busy = env
            .platform
            .sockets()
            .is_ok_and(|ss| ss.iter().any(|s| s.local.port() == port));
        if !busy {
            break true;
        }
        let elapsed = start.elapsed();
        if elapsed >= timeout {
            break false;
        }
        std::thread::sleep(
            timeout
                .saturating_sub(elapsed)
                .min(Duration::from_millis(250)),
        );
    };
    let waited_ms = u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX);
    if env.json {
        emit_json(
            env.out,
            &WaitOut {
                schema: SCHEMA,
                port,
                free,
                waited_ms,
            },
        );
    } else if free {
        let _ = writeln!(env.out, "Port {port} is free");
    } else {
        let _ = writeln!(
            env.out,
            "Port {port} is still in use after {}",
            humanize(timeout)
        );
    }
    if free { 0 } else { 4 }
}

fn humanize(d: Duration) -> String {
    if d.as_millis() % 1000 == 0 {
        format!("{}s", d.as_secs())
    } else {
        format!("{}ms", d.as_millis())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use atropos_core::platform::{FakePlatform, RawProc, Reaction};
    use atropos_core::ports::Socket;

    const ME: ProcKey = ProcKey {
        pid: 3,
        create_time: 3,
    };

    fn k(pid: u32) -> ProcKey {
        ProcKey {
            pid,
            create_time: u64::from(pid),
        }
    }

    /// terminal(2) → atr(3); node(10) → node(11) listening on 3000; lsass(20).
    fn fake() -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(2), 1, "WindowsTerminal.exe"),
            RawProc::new(ME, 2, "atr.exe"),
            RawProc::new(k(10), 1, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(20), 1, "lsass.exe"),
        ]);
        f.sockets = vec![Socket {
            proto: Proto::Tcp,
            local: "127.0.0.1:3000".parse().unwrap(),
            remote: None,
            state: Some(TcpState::Listen),
            owner: 11,
        }];
        f
    }

    struct Run {
        code: u8,
        out: String,
    }

    fn run(
        f: &mut FakePlatform,
        json: bool,
        yes_to_prompts: bool,
        cmd: impl FnOnce(&mut Env<'_>) -> u8,
    ) -> Run {
        let mut out = Vec::new();
        let mut confirm = |_: &str| yes_to_prompts;
        let code = {
            let mut env = Env {
                platform: f,
                me: ME,
                out: &mut out,
                json,
                confirm: &mut confirm,
            };
            cmd(&mut env)
        };
        Run {
            code,
            out: String::from_utf8(out).unwrap(),
        }
    }

    fn yes() -> KillArgs {
        KillArgs {
            yes: true,
            ..KillArgs::default()
        }
    }

    #[test]
    fn durations() {
        assert_eq!(parse_duration("500ms"), Ok(Duration::from_millis(500)));
        assert_eq!(parse_duration("5s"), Ok(Duration::from_secs(5)));
        assert_eq!(parse_duration("2m"), Ok(Duration::from_secs(120)));
        assert_eq!(parse_duration("30"), Ok(Duration::from_secs(30)));
        assert!(parse_duration("soon").is_err());
        assert!(parse_duration("").is_err());
    }

    #[test]
    fn port_found_json_golden() {
        let r = run(&mut fake(), true, false, |e| port(e, 3000, None));
        assert_eq!(r.code, 0);
        assert_eq!(
            r.out,
            "{\"schema\":1,\"port\":3000,\"owners\":[{\"pid\":11,\"create_time\":11,\"name\":\"node.exe\",\"proto\":\"tcp\",\"local\":\"127.0.0.1:3000\",\"state\":\"listen\",\"badge\":null}]}\n"
        );
    }

    #[test]
    fn port_none_exits_1() {
        let r = run(&mut fake(), true, false, |e| port(e, 9999, None));
        assert_eq!(r.code, 1);
        assert_eq!(r.out, "{\"schema\":1,\"port\":9999,\"owners\":[]}\n");
        let human = run(&mut fake(), false, false, |e| port(e, 9999, None));
        assert!(human.out.contains("Nothing is using port 9999"));
    }

    #[test]
    fn port_kill_exit_codes() {
        let mut f = fake();
        assert_eq!(
            run(&mut f, false, false, |e| port(e, 3000, Some(yes()))).code,
            0
        );
        assert_eq!(f.terminate_calls(), 1);

        let mut f = fake();
        f.set_reaction(k(11), Reaction::Unkillable);
        assert_eq!(
            run(&mut f, false, false, |e| port(e, 3000, Some(yes()))).code,
            3
        );

        let mut f = fake();
        f.deny(k(11));
        assert_eq!(
            run(&mut f, false, false, |e| port(e, 3000, Some(yes()))).code,
            2
        );
    }

    #[test]
    fn dry_run_never_touches_anything() {
        let mut f = fake();
        let args = KillArgs {
            dry_run: true,
            tree: true,
            ..KillArgs::default()
        };
        let r = run(&mut f, false, false, |e| port(e, 3000, Some(args)));
        assert_eq!(r.code, 0);
        assert!(r.out.contains("Would end node.exe (11)"), "{}", r.out);
        assert_eq!(f.terminate_calls() + f.graceful_calls(), 0);
        assert_eq!(f.suspend_log(), &[] as &[ProcKey]);
        let r = run(&mut f, true, false, |e| kill_cmd(e, "20", args));
        assert!(r.out.contains("\"outcome\":\"refused\""), "{}", r.out);
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn kill_needs_confirmation_and_refuses_when_declined() {
        let mut f = fake();
        let r = run(&mut f, false, false, |e| {
            kill_cmd(e, "10", KillArgs::default())
        });
        assert_eq!(r.code, 1, "declined");
        assert_eq!(f.terminate_calls(), 0);
        let r = run(&mut f, false, true, |e| {
            kill_cmd(e, "10", KillArgs::default())
        });
        assert_eq!(r.code, 0, "accepted at the prompt");
    }

    #[test]
    fn kill_protected_and_ancestors_exit_2() {
        let mut f = fake();
        let r = run(&mut f, true, false, |e| kill_cmd(e, "20", yes()));
        assert_eq!(r.code, 2);
        assert!(r.out.contains("\"outcome\":\"protected\""));
        assert_eq!(
            run(&mut f, false, false, |e| kill_cmd(e, "2", yes())).code,
            2
        );
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn kill_unknown_exits_1_and_query_kills_all_matches() {
        let mut f = fake();
        assert_eq!(
            run(&mut f, false, false, |e| kill_cmd(e, "4242", yes())).code,
            1
        );
        assert_eq!(
            run(&mut f, false, false, |e| kill_cmd(e, "name:python", yes())).code,
            1
        );
        let r = run(&mut f, true, false, |e| kill_cmd(e, "name:node", yes()));
        assert_eq!(r.code, 0);
        assert_eq!(r.out.matches("\"outcome\":\"terminated\"").count(), 2);
    }

    #[test]
    fn kill_tree_reports_each_member() {
        let mut f = fake();
        let args = KillArgs {
            tree: true,
            ..yes()
        };
        let r = run(&mut f, true, false, |e| kill_cmd(e, "10", args));
        assert_eq!(r.code, 0);
        assert!(r.out.contains("\"pid\":10") && r.out.contains("\"pid\":11"));
    }

    #[test]
    fn tree_kill_skips_protected_descendants() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(ME, 1, "atr.exe"),
            RawProc::new(k(10), 1, "node.exe"),
            RawProc::new(k(11), 10, "csrss.exe"),
            RawProc::new(k(12), 10, "node.exe"),
        ]);
        let args = KillArgs {
            tree: true,
            ..yes()
        };
        let r = run(&mut f, true, false, |e| kill_cmd(e, "10", args));
        assert!(
            r.out.contains(
                "\"pid\":11,\"create_time\":11,\"name\":\"csrss.exe\",\"outcome\":\"protected\""
            ),
            "{}",
            r.out
        );
        assert_eq!(r.code, 2);
    }

    #[test]
    fn console_scope_keeps_the_shell() {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(ME, 1, "atr.exe"),
            RawProc::new(k(5), 1, "pwsh.exe"),
            RawProc::new(k(10), 5, "node.exe"),
            RawProc::new(k(11), 10, "node.exe"),
            RawProc::new(k(12), 1, "node.exe"),
        ]);
        f.set_console(k(10), vec![5, 10, 11, 12, 3]);
        let args = KillArgs {
            console: true,
            ..yes()
        };
        let r = run(&mut f, true, false, |e| kill_cmd(e, "10", args));
        assert_eq!(r.code, 0, "{}", r.out);
        for pid in [10, 11, 12] {
            assert!(
                r.out.contains(&format!("\"pid\":{pid},")),
                "{pid}: {}",
                r.out
            );
        }
        assert!(
            !r.out.contains("\"pid\":5,") && !r.out.contains("\"pid\":3,"),
            "{}",
            r.out
        );
    }

    #[test]
    fn find_matches_and_never_lists_atr_itself() {
        let r = run(&mut fake(), true, false, |e| find(e, "name:node"));
        assert_eq!(r.code, 0);
        assert_eq!(r.out.matches("\"name\":\"node.exe\"").count(), 2);
        assert_eq!(
            run(&mut fake(), false, false, |e| find(e, "name:atr")).code,
            1
        );
        assert_eq!(run(&mut fake(), false, false, |e| find(e, "zzzz")).code, 1);
    }

    #[test]
    fn tree_prints_indented_children() {
        let r = run(&mut fake(), false, false, |e| tree(e, Some(10)));
        assert_eq!(r.code, 0);
        assert!(
            r.out.contains("node.exe (10)\n  node.exe (11)"),
            "{}",
            r.out
        );
        let j = run(&mut fake(), true, false, |e| tree(e, Some(10)));
        assert!(
            j.out.starts_with("{\"schema\":1,\"roots\":[{\"pid\":10,"),
            "{}",
            j.out
        );
    }

    #[test]
    fn wait_port_free_and_timeout() {
        let r = run(&mut fake(), true, false, |e| {
            wait_port(e, 9999, Duration::from_millis(100))
        });
        assert_eq!(r.code, 0);
        assert!(r.out.contains("\"free\":true"));
        let r = run(&mut fake(), false, false, |e| {
            wait_port(e, 3000, Duration::from_millis(300))
        });
        assert_eq!(r.code, 4);
    }
}
