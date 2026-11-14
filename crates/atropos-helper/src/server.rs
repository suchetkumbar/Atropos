//! Request handling inside the helper: validate the frame and the nonce, re-check the policy on a
//! fresh scan (the same protected list as the GUI), then act through a fresh pin. Every request is
//! logged with its outcome.

use std::sync::Arc;

use atropos_core::ProcKey;
use atropos_core::engine::kill::{KillOpts, Strategy, kill};
use atropos_core::engine::policy::{self, ActionKind, Context, Verdict};
use atropos_core::engine::suspend::{SuspendLedger, Toggle};
use atropos_core::pin::Access;
use atropos_core::platform::Platform;
use atropos_core::snapshot::build;
use atropos_core::statics::StaticsMap;

use crate::protocol::{NONCE_LEN, Op, Rejection, Reply, decode_request};

/// Compares nonces in constant time.
#[must_use]
pub fn nonce_matches(a: &[u8; NONCE_LEN], b: &[u8; NONCE_LEN]) -> bool {
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

/// Handles one request body. `me` = the helper's own key (it and its ancestors, i.e. the GUI,
/// are protected).
pub fn handle(
    platform: &mut dyn Platform,
    ledger: &mut SuspendLedger,
    me: ProcKey,
    nonce: &[u8; NONCE_LEN],
    body: &[u8],
) -> Reply {
    let req = match decode_request(body) {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!("helper: rejected frame: {e}");
            return Reply::Rejected(Rejection::BadFrame);
        }
    };
    if !nonce_matches(&req.nonce, nonce) {
        tracing::warn!("helper: rejected {:?} on {}: bad nonce", req.op, req.key);
        return Reply::Rejected(Rejection::BadNonce);
    }
    let key = req.key;
    let kind = match req.op {
        Op::Suspend => ActionKind::Suspend,
        Op::Resume => ActionKind::Resume,
        Op::Terminate | Op::GracefulClose | Op::StopService(_) => ActionKind::Kill,
    };
    // Same protected list as the GUI, on a fresh scan.
    let mut raw = Vec::new();
    let snap = match platform.scan(&mut raw) {
        Ok(stats) => build(None, raw, stats),
        Err(_) => atropos_core::snapshot::Snapshot::default(),
    };
    let services = platform.services().unwrap_or_default();
    let mut statics = StaticsMap::new();
    if snap.index.contains_key(&key) {
        statics.insert(key, Arc::new(platform.statics(key)));
    }
    let ctx = Context {
        snapshot: &snap,
        statics: &statics,
        services: &services,
        me,
    };
    let hard = matches!(policy::check(&ctx, key, kind), Verdict::Hard(_));
    let reply = match req.op {
        Op::Suspend | Op::Resume => {
            let r = if hard {
                vec![(key, Toggle::Protected)]
            } else if req.op == Op::Suspend {
                ledger.suspend(platform, &snap, key, false, &|_| false)
            } else {
                ledger.resume(platform, &snap, key, false, &|_| false)
            };
            Reply::Toggled(r.first().map_or(Toggle::Gone, |(_, t)| *t))
        }
        Op::Terminate => {
            let opts = KillOpts {
                force: true,
                ..KillOpts::default()
            };
            Reply::Killed(kill(platform, key, opts, hard, &mut |_| {}))
        }
        Op::GracefulClose => {
            Reply::Killed(kill(platform, key, KillOpts::default(), hard, &mut |_| {}))
        }
        Op::StopService(ref name) => {
            use atropos_core::engine::kill::Outcome;
            use atropos_core::pin::PinError;
            if hard {
                Reply::Killed(Outcome::Protected)
            } else {
                match platform.pin(key, Access::KILL) {
                    Err(PinError::Gone | PinError::Reused) => Reply::Killed(Outcome::Gone),
                    Err(_) => Reply::Killed(Outcome::Denied),
                    Ok(p) => {
                        let how = Strategy::Service(vec![name.clone()]);
                        if platform.graceful(&p, &how).is_ok() {
                            Reply::Killed(Outcome::Exited)
                        } else {
                            Reply::Killed(Outcome::Denied)
                        }
                    }
                }
            }
        }
    };
    tracing::info!("helper: {:?} on {key} -> {reply:?}", req.op);
    reply
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::{Request, encode_request};
    use atropos_core::engine::kill::Outcome;
    use atropos_core::platform::{FakePlatform, RawProc};

    const NONCE: [u8; NONCE_LEN] = [42; NONCE_LEN];
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

    fn fake() -> FakePlatform {
        let mut f = FakePlatform::new(1);
        f.push_frame(vec![
            RawProc::new(k(2), 1, "Atropos.exe"),
            RawProc::new(ME, 2, "Atropos.exe"),
            RawProc::new(k(10), 1, "elevated-tool.exe"),
            RawProc::new(k(20), 1, "lsass.exe"),
        ]);
        f
    }

    fn send(f: &mut FakePlatform, nonce: [u8; NONCE_LEN], op: Op, key: ProcKey) -> Reply {
        let frame = encode_request(&Request { nonce, op, key });
        handle(f, &mut SuspendLedger::default(), ME, &NONCE, &frame[4..])
    }

    #[test]
    fn nonce_compare() {
        assert!(nonce_matches(&NONCE, &NONCE));
        let mut other = NONCE;
        other[31] ^= 1;
        assert!(!nonce_matches(&NONCE, &other));
    }

    #[test]
    fn bad_nonce_is_rejected_without_acting() {
        let mut f = fake();
        let r = send(&mut f, [0; NONCE_LEN], Op::Terminate, k(10));
        assert_eq!(r, Reply::Rejected(Rejection::BadNonce));
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn malformed_frame_is_rejected() {
        let mut f = fake();
        let r = handle(
            &mut f,
            &mut SuspendLedger::default(),
            ME,
            &NONCE,
            &[1, 2, 3],
        );
        assert_eq!(r, Reply::Rejected(Rejection::BadFrame));
    }

    #[test]
    fn protected_keys_are_refused_inside_the_helper() {
        let mut f = fake();
        assert_eq!(
            send(&mut f, NONCE, Op::Terminate, k(20)),
            Reply::Killed(Outcome::Protected)
        );
        // The GUI that launched the helper (its parent) too.
        assert_eq!(
            send(&mut f, NONCE, Op::GracefulClose, k(2)),
            Reply::Killed(Outcome::Protected)
        );
        assert_eq!(
            send(&mut f, NONCE, Op::Suspend, k(20)),
            Reply::Toggled(Toggle::Protected)
        );
        assert_eq!(f.terminate_calls(), 0);
    }

    #[test]
    fn allowed_ops_act_through_a_fresh_pin() {
        let mut f = fake();
        assert_eq!(
            send(&mut f, NONCE, Op::Suspend, k(10)),
            Reply::Toggled(Toggle::Done)
        );
        assert_eq!(f.suspend_count(k(10)), 1);
        assert_eq!(
            send(&mut f, NONCE, Op::Resume, k(10)),
            Reply::Toggled(Toggle::Done)
        );
        assert_eq!(
            send(&mut f, NONCE, Op::Terminate, k(10)),
            Reply::Killed(Outcome::Terminated)
        );
        // The PID is gone now; a stale request must not hit anything else.
        assert_eq!(
            send(&mut f, NONCE, Op::Terminate, k(10)),
            Reply::Killed(Outcome::Gone)
        );
    }

    #[test]
    fn reused_pid_is_gone() {
        let mut f = fake();
        let stale = ProcKey {
            pid: 10,
            create_time: 1,
        };
        assert_eq!(
            send(&mut f, NONCE, Op::Terminate, stale),
            Reply::Killed(Outcome::Gone)
        );
        assert_eq!(f.terminate_calls(), 0);
    }
}
