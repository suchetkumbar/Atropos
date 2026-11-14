//! The helper process: `Atropos.exe --elevated-helper --pipe <name> --nonce <64 hex>
//! --user-sid <S-1-…> --parent <pid>:<create_time> [--idle-secs <n>]`.
//!
//! Serves one client at a time on a pipe only `--user-sid` may open; checks the client's SID
//! again on every connection; exits after `--idle-secs` (default 300) without requests or as soon
//! as the parent process exits. Nothing is written except log lines.

use std::process::ExitCode;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::engine::suspend::SuspendLedger;
use atropos_core::pin::Access;
use atropos_core::platform::Platform;
use atropos_core::platform::windows::pipe::PipeServer;
use atropos_core::platform::windows::{WindowsPlatform, current_process_key};

use crate::protocol::{NONCE_LEN, Rejection, Reply, encode_reply, read_frame};
use crate::server::handle;

/// Default idle lifetime (spec: 5 minutes).
pub const IDLE: Duration = Duration::from_secs(300);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HelperArgs {
    pub pipe: String,
    pub nonce: [u8; NONCE_LEN],
    pub user_sid: String,
    pub parent: ProcKey,
    pub idle: Duration,
}

/// Lowercase hex.
#[must_use]
pub fn hex(b: &[u8]) -> String {
    use std::fmt::Write as _;
    b.iter()
        .fold(String::with_capacity(b.len() * 2), |mut s, x| {
            let _ = write!(s, "{x:02x}");
            s
        })
}

/// Parses the helper's arguments (everything after the program name). `None` if anything is
/// missing or malformed: the helper then exits without serving.
#[must_use]
pub fn parse_args(args: &[String]) -> Option<HelperArgs> {
    let get = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .map(String::as_str)
    };
    let pipe = get("--pipe")?;
    if pipe.is_empty()
        || pipe.len() > 200
        || !pipe.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    {
        return None;
    }
    let hexs = get("--nonce")?;
    if hexs.len() != NONCE_LEN * 2 {
        return None;
    }
    let mut nonce = [0u8; NONCE_LEN];
    for (i, b) in nonce.iter_mut().enumerate() {
        *b = u8::from_str_radix(hexs.get(i * 2..i * 2 + 2)?, 16).ok()?;
    }
    let user_sid = get("--user-sid")?;
    if !user_sid.starts_with("S-1-")
        || !user_sid[4..]
            .chars()
            .all(|c| c.is_ascii_digit() || c == '-')
    {
        return None;
    }
    let (pid, ct) = get("--parent")?.split_once(':')?;
    let parent = ProcKey {
        pid: pid.parse().ok()?,
        create_time: ct.parse().ok()?,
    };
    let idle = match get("--idle-secs") {
        Some(s) => Duration::from_secs(s.parse().ok()?),
        None => IDLE,
    };
    Some(HelperArgs {
        pipe: pipe.to_owned(),
        nonce,
        user_sid: user_sid.to_owned(),
        parent,
        idle,
    })
}

/// Runs the helper until idle or the parent exits.
pub fn run(args: &HelperArgs) -> ExitCode {
    let Ok(me) = current_process_key() else {
        return ExitCode::FAILURE;
    };
    let server = match PipeServer::create(&args.pipe, &args.user_sid) {
        Ok(s) => s,
        Err(e) => {
            tracing::error!("helper: cannot create pipe: {e}");
            return ExitCode::FAILURE;
        }
    };
    tracing::info!(
        "helper: serving for {} (parent {})",
        args.user_sid,
        args.parent
    );
    let start = Instant::now();
    let last = Arc::new(AtomicU64::new(0));
    spawn_watchdog(args.parent, args.idle, start, Arc::clone(&last));
    let mut platform = WindowsPlatform::new();
    let mut ledger = SuspendLedger::default();
    loop {
        let Ok(mut conn) = server.accept() else {
            continue;
        };
        // Defence in depth: the DACL already admits only this SID.
        if conn.client_sid().ok().as_deref() != Some(args.user_sid.as_str()) {
            tracing::warn!("helper: rejected a client with the wrong user");
            let _ = std::io::Write::write_all(
                &mut conn,
                &encode_reply(Reply::Rejected(Rejection::WrongUser)),
            );
            continue;
        }
        while let Ok(body) = read_frame(&mut conn) {
            let reply = handle(&mut platform, &mut ledger, me, &args.nonce, &body);
            last.store(elapsed_ms(start), Ordering::Relaxed);
            if std::io::Write::write_all(&mut conn, &encode_reply(reply)).is_err() {
                break;
            }
            if matches!(reply, Reply::Rejected(_)) {
                break; // a bad nonce or frame ends the session
            }
        }
    }
}

fn elapsed_ms(start: Instant) -> u64 {
    u64::try_from(start.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// Exits the process when the parent exits or after `idle` without requests.
fn spawn_watchdog(parent: ProcKey, idle: Duration, start: Instant, last: Arc<AtomicU64>) {
    std::thread::spawn(move || {
        let mut platform = WindowsPlatform::new();
        let pin = platform.pin(parent, Access::QUERY).ok();
        loop {
            let parent_gone = pin
                .as_ref()
                .is_none_or(|p| platform.wait_exits(&[p], Duration::from_millis(500))[0]);
            let idle_ms = elapsed_ms(start).saturating_sub(last.load(Ordering::Relaxed));
            if parent_gone || idle_ms >= u64::try_from(idle.as_millis()).unwrap_or(u64::MAX) {
                tracing::info!("helper: exiting (parent gone: {parent_gone})");
                std::process::exit(0);
            }
            if pin.is_none() {
                std::thread::sleep(Duration::from_millis(500));
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| (*s).to_owned()).collect()
    }

    fn good() -> Vec<String> {
        a(&[
            "--elevated-helper",
            "--pipe",
            "atropos-abc",
            "--nonce",
            &"ab".repeat(32),
            "--user-sid",
            "S-1-5-21-1-2-3-1001",
            "--parent",
            "4242:133000000000000001",
        ])
    }

    #[test]
    fn parses_all_arguments() {
        let h = parse_args(&good()).unwrap();
        assert_eq!(h.pipe, "atropos-abc");
        assert_eq!(h.nonce, [0xab; NONCE_LEN]);
        assert_eq!(h.user_sid, "S-1-5-21-1-2-3-1001");
        assert_eq!(
            h.parent,
            ProcKey {
                pid: 4242,
                create_time: 133_000_000_000_000_001
            }
        );
        assert_eq!(h.idle, IDLE);
        let mut v = good();
        v.extend(a(&["--idle-secs", "2"]));
        assert_eq!(parse_args(&v).unwrap().idle, Duration::from_secs(2));
    }

    #[test]
    fn rejects_missing_or_malformed_arguments() {
        for (i, bad) in [
            (4, "abcd"),           // nonce too short
            (4, &"zz".repeat(32)), // not hex
            (6, "Everyone"),       // not a SID
            (8, "4242"),           // no create time
            (2, "bad\\name"),      // pipe name with a path separator
        ] {
            let mut v = good();
            v[i] = bad.to_owned();
            assert_eq!(parse_args(&v), None, "{bad}");
        }
        assert_eq!(parse_args(&a(&["--elevated-helper"])), None);
    }

    #[test]
    fn hex_round_trips() {
        assert_eq!(hex(&[0, 15, 255]), "000fff");
    }
}
