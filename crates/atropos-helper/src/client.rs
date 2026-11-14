//! The GUI's side: start the helper elevated (one UAC prompt) and talk to it over its pipe.

use std::io::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use atropos_core::ProcKey;
use atropos_core::platform::windows::pipe::{current_user_sid, random_bytes, run_elevated};

use crate::host::hex;
use crate::protocol::{NONCE_LEN, Op, Reply, Request, decode_reply, encode_request, read_frame};

/// How long to wait for the helper's pipe (the user may take a while at the UAC prompt).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(120);

const ERROR_FILE_NOT_FOUND: i32 = 2;
const ERROR_PIPE_BUSY: i32 = 231;

/// An open connection to a running helper.
pub struct Session {
    pipe: std::fs::File,
    nonce: [u8; NONCE_LEN],
}

/// Connects to helper pipe `name`, retrying until it exists or `timeout` passes.
///
/// # Errors
/// If the pipe never appears or refuses us.
pub fn connect(name: &str, nonce: [u8; NONCE_LEN], timeout: Duration) -> std::io::Result<Session> {
    let path = format!(r"\\.\pipe\{name}");
    let start = Instant::now();
    loop {
        match std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)
        {
            Ok(pipe) => return Ok(Session { pipe, nonce }),
            Err(e)
                if matches!(
                    e.raw_os_error(),
                    Some(ERROR_FILE_NOT_FOUND | ERROR_PIPE_BUSY)
                ) && start.elapsed() < timeout =>
            {
                std::thread::sleep(Duration::from_millis(100));
            }
            Err(e) => return Err(e),
        }
    }
}

/// Starts `atropos_exe --elevated-helper …` through UAC and connects to it.
///
/// # Errors
/// If the user declines the UAC prompt, or the helper never answers.
pub fn launch(atropos_exe: &Path, parent: ProcKey) -> std::io::Result<Session> {
    let other = |e: atropos_core::CoreError| std::io::Error::other(e.to_string());
    let name = format!("atropos-{}", hex(&random_bytes::<16>().map_err(other)?));
    let nonce = random_bytes::<NONCE_LEN>().map_err(other)?;
    let sid = current_user_sid().map_err(other)?;
    let args = format!(
        "--elevated-helper --pipe {name} --nonce {} --user-sid {sid} --parent {}:{}",
        hex(&nonce),
        parent.pid,
        parent.create_time
    );
    run_elevated(atropos_exe, &args).map_err(other)?;
    connect(&name, nonce, CONNECT_TIMEOUT)
}

impl Session {
    /// Sends one request and waits for the reply.
    ///
    /// # Errors
    /// If the pipe breaks or the reply is malformed.
    pub fn send(&mut self, op: Op, key: ProcKey) -> std::io::Result<Reply> {
        let frame = encode_request(&Request {
            nonce: self.nonce,
            op,
            key,
        });
        self.pipe.write_all(&frame)?;
        let body = read_frame(&mut self.pipe)?;
        decode_reply(&body).map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))
    }

    /// For tests: send with a different nonce.
    #[doc(hidden)]
    #[must_use]
    pub fn with_nonce(mut self, nonce: [u8; NONCE_LEN]) -> Self {
        self.nonce = nonce;
        self
    }
}
