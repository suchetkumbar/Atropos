//! Wire format between the GUI and the elevated helper. One request frame, one reply frame:
//!
//! ```text
//! request: u32 len | u8 version | [u8; 32] nonce | u8 op | u32 pid | u64 create_time | u16 n | n bytes (service name, UTF-8)
//! reply:   u32 len | u8 version | u8 kind | u8 value
//! ```
//! All integers little-endian. Frames over [`MAX_FRAME`] bytes are rejected unread.

use atropos_core::ProcKey;
use atropos_core::engine::kill::Outcome;
use atropos_core::engine::suspend::Toggle;

pub const VERSION: u8 = 1;
pub const NONCE_LEN: usize = 32;
/// Largest frame body accepted (service names are short).
pub const MAX_FRAME: usize = 1024;

/// The only operations the helper performs (spec §4.7).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Terminate,
    Suspend,
    Resume,
    StopService(String),
    GracefulClose,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub nonce: [u8; NONCE_LEN],
    pub op: Op,
    pub key: ProcKey,
}

/// Why the helper refused a request before acting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    BadNonce,
    BadFrame,
    WrongUser,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reply {
    Killed(Outcome),
    Toggled(Toggle),
    Rejected(Rejection),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum FrameError {
    #[error("frame too short")]
    Short,
    #[error("frame too large")]
    TooLarge,
    #[error("unsupported protocol version")]
    Version,
    #[error("malformed frame")]
    Malformed,
}

const HEADER: usize = 1 + NONCE_LEN + 1 + 4 + 8 + 2;

fn op_code(op: &Op) -> u8 {
    match op {
        Op::Terminate => 0,
        Op::Suspend => 1,
        Op::Resume => 2,
        Op::StopService(_) => 3,
        Op::GracefulClose => 4,
    }
}

const OUTCOMES: [Outcome; 6] = [
    Outcome::Exited,
    Outcome::Terminated,
    Outcome::Denied,
    Outcome::Protected,
    Outcome::Gone,
    Outcome::Stuck,
];
const TOGGLES: [Toggle; 4] = [
    Toggle::Done,
    Toggle::Denied,
    Toggle::Protected,
    Toggle::Gone,
];
const REJECTIONS: [Rejection; 3] = [
    Rejection::BadNonce,
    Rejection::BadFrame,
    Rejection::WrongUser,
];

fn index_of<T: PartialEq>(all: &[T], v: &T) -> u8 {
    u8::try_from(all.iter().position(|x| x == v).unwrap_or(0)).unwrap_or(0)
}

fn framed(body: Vec<u8>) -> Vec<u8> {
    let len = u32::try_from(body.len()).unwrap_or(u32::MAX);
    let mut f = len.to_le_bytes().to_vec();
    f.extend(body);
    f
}

/// Encodes a request as a length-prefixed frame.
#[must_use]
pub fn encode_request(r: &Request) -> Vec<u8> {
    let name = match &r.op {
        Op::StopService(n) => n.as_bytes(),
        _ => &[],
    };
    let mut b = Vec::with_capacity(HEADER + name.len());
    b.push(VERSION);
    b.extend_from_slice(&r.nonce);
    b.push(op_code(&r.op));
    b.extend_from_slice(&r.key.pid.to_le_bytes());
    b.extend_from_slice(&r.key.create_time.to_le_bytes());
    b.extend_from_slice(&u16::try_from(name.len()).unwrap_or(0).to_le_bytes());
    b.extend_from_slice(name);
    framed(b)
}

/// Decodes a request body (the bytes after the length prefix).
///
/// # Errors
/// On any malformed or oversized body.
pub fn decode_request(body: &[u8]) -> Result<Request, FrameError> {
    if body.len() > MAX_FRAME {
        return Err(FrameError::TooLarge);
    }
    if body.len() < HEADER {
        return Err(FrameError::Short);
    }
    if body[0] != VERSION {
        return Err(FrameError::Version);
    }
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&body[1..=NONCE_LEN]);
    let mut at = 1 + NONCE_LEN;
    let op = body[at];
    at += 1;
    let mut pid = [0u8; 4];
    pid.copy_from_slice(&body[at..at + 4]);
    at += 4;
    let mut ct = [0u8; 8];
    ct.copy_from_slice(&body[at..at + 8]);
    at += 8;
    let n = usize::from(u16::from_le_bytes([body[at], body[at + 1]]));
    at += 2;
    if body.len() != at + n {
        return Err(FrameError::Malformed);
    }
    let name = &body[at..];
    let op = match (op, n) {
        (0, 0) => Op::Terminate,
        (1, 0) => Op::Suspend,
        (2, 0) => Op::Resume,
        (4, 0) => Op::GracefulClose,
        (3, 1..) => Op::StopService(
            std::str::from_utf8(name)
                .map_err(|_| FrameError::Malformed)?
                .to_owned(),
        ),
        _ => return Err(FrameError::Malformed),
    };
    Ok(Request {
        nonce,
        op,
        key: ProcKey {
            pid: u32::from_le_bytes(pid),
            create_time: u64::from_le_bytes(ct),
        },
    })
}

#[must_use]
pub fn encode_reply(r: Reply) -> Vec<u8> {
    let (kind, value) = match r {
        Reply::Killed(o) => (0, index_of(&OUTCOMES, &o)),
        Reply::Toggled(t) => (1, index_of(&TOGGLES, &t)),
        Reply::Rejected(x) => (2, index_of(&REJECTIONS, &x)),
    };
    framed(vec![VERSION, kind, value])
}

/// # Errors
/// On any malformed body.
pub fn decode_reply(body: &[u8]) -> Result<Reply, FrameError> {
    let [v, kind, value] = body else {
        return Err(if body.len() < 3 {
            FrameError::Short
        } else {
            FrameError::Malformed
        });
    };
    if *v != VERSION {
        return Err(FrameError::Version);
    }
    let i = usize::from(*value);
    match kind {
        0 => OUTCOMES.get(i).map(|o| Reply::Killed(*o)),
        1 => TOGGLES.get(i).map(|t| Reply::Toggled(*t)),
        2 => REJECTIONS.get(i).map(|x| Reply::Rejected(*x)),
        _ => None,
    }
    .ok_or(FrameError::Malformed)
}

/// Reads one length-prefixed frame body.
///
/// # Errors
/// I/O errors, or `InvalidData` for a frame over [`MAX_FRAME`].
pub fn read_frame(r: &mut impl std::io::Read) -> std::io::Result<Vec<u8>> {
    let mut len = [0u8; 4];
    r.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len) as usize;
    if len > MAX_FRAME {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "frame too large",
        ));
    }
    let mut body = vec![0u8; len];
    r.read_exact(&mut body)?;
    Ok(body)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k() -> ProcKey {
        ProcKey {
            pid: 4242,
            create_time: 133_000_000_000_000_001,
        }
    }

    fn body(frame: &[u8]) -> &[u8] {
        &frame[4..]
    }

    #[test]
    fn requests_round_trip() {
        for op in [
            Op::Terminate,
            Op::Suspend,
            Op::Resume,
            Op::GracefulClose,
            Op::StopService("Spöoler".into()),
        ] {
            let r = Request {
                nonce: [7; NONCE_LEN],
                op,
                key: k(),
            };
            let f = encode_request(&r);
            assert_eq!(
                u32::from_le_bytes([f[0], f[1], f[2], f[3]]) as usize,
                f.len() - 4
            );
            assert_eq!(decode_request(body(&f)), Ok(r));
        }
    }

    #[test]
    fn replies_round_trip() {
        for r in [
            Reply::Killed(Outcome::Terminated),
            Reply::Killed(Outcome::Stuck),
            Reply::Toggled(Toggle::Done),
            Reply::Toggled(Toggle::Protected),
            Reply::Rejected(Rejection::BadNonce),
            Reply::Rejected(Rejection::WrongUser),
        ] {
            assert_eq!(decode_reply(body(&encode_reply(r))), Ok(r));
        }
    }

    #[test]
    fn malformed_frames_are_rejected() {
        let good = encode_request(&Request {
            nonce: [1; NONCE_LEN],
            op: Op::Terminate,
            key: k(),
        });
        let b = body(&good);
        assert_eq!(decode_request(&b[..10]), Err(FrameError::Short));
        let mut v = b.to_vec();
        v[0] = 99;
        assert_eq!(decode_request(&v), Err(FrameError::Version));
        let mut v = b.to_vec();
        v[1 + NONCE_LEN] = 200; // unknown op
        assert_eq!(decode_request(&v), Err(FrameError::Malformed));
        let mut v = b.to_vec();
        v.push(0); // trailing garbage
        assert_eq!(decode_request(&v), Err(FrameError::Malformed));
        assert_eq!(
            decode_request(&vec![VERSION; MAX_FRAME + 1]),
            Err(FrameError::TooLarge)
        );
        assert_eq!(decode_reply(&[VERSION, 9, 0]), Err(FrameError::Malformed));
    }

    #[test]
    fn read_frame_refuses_oversized_lengths() {
        let mut huge = std::io::Cursor::new(u32::MAX.to_le_bytes().to_vec());
        assert!(read_frame(&mut huge).is_err());
        let f = encode_reply(Reply::Toggled(Toggle::Done));
        let mut c = std::io::Cursor::new(f.clone());
        assert_eq!(read_frame(&mut c).unwrap(), f[4..].to_vec());
    }
}
