//! Win32 pieces of the elevation helper (spec §4.7): a single-instance named pipe whose DACL
//! grants exactly one SID, the connected client's SID, a CSPRNG nonce, and `runas` launching.

use std::path::Path;

use windows::Win32::Foundation::{HANDLE, HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertSidToStringSidW,
    ConvertStringSecurityDescriptorToSecurityDescriptorW, GetSecurityInfo, SDDL_REVISION_1,
    SE_KERNEL_OBJECT,
};
use windows::Win32::Security::Cryptography::{BCRYPT_USE_SYSTEM_PREFERRED_RNG, BCryptGenRandom};
use windows::Win32::Security::{
    DACL_SECURITY_INFORMATION, GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES,
    TOKEN_QUERY, TOKEN_USER, TokenUser,
};
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_FIRST_PIPE_INSTANCE, FlushFileBuffers, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, GetNamedPipeClientProcessId,
    PIPE_READMODE_BYTE, PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows::Win32::System::Threading::{
    GetCurrentProcess, OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION,
};
use windows::Win32::UI::Shell::{SEE_MASK_NOASYNC, SHELLEXECUTEINFOW, ShellExecuteExW};
use windows::Win32::UI::WindowsAndMessaging::SW_HIDE;
use windows::core::{HSTRING, PWSTR, w};

use super::handle::OwnedHandle;
use crate::{CoreError, Result};

const ERROR_PIPE_CONNECTED: i32 = 535;
const ERROR_BROKEN_PIPE: i32 = 109;

fn os(api: &'static str, e: &windows::core::Error) -> CoreError {
    CoreError::Os {
        api,
        code: e.code().0 & 0xFFFF,
    }
}

/// Takes ownership of a `LocalAlloc`ed wide string and frees it.
fn take_local_string(p: PWSTR) -> String {
    // SAFETY: `p` is a NUL-terminated string allocated by the API with LocalAlloc.
    let s = unsafe { p.to_string() }.unwrap_or_default();
    // SAFETY: freeing exactly the allocation the API handed us.
    unsafe {
        LocalFree(Some(HLOCAL(p.0.cast())));
    }
    s
}

/// String SID of the user a process token belongs to.
fn token_sid(process: HANDLE) -> Result<String> {
    let mut tok = HANDLE::default();
    // SAFETY: valid out-pointer; the token is owned by `OwnedHandle`.
    unsafe { OpenProcessToken(process, TOKEN_QUERY, &raw mut tok) }
        .map_err(|e| os("OpenProcessToken", &e))?;
    let tok = OwnedHandle::new(tok);
    let mut buf = vec![0u64; 16];
    let mut ret = 0u32;
    for _ in 0..2 {
        let cap = u32::try_from(buf.len() * 8).unwrap_or(u32::MAX);
        // SAFETY: `buf` is 8-aligned and `cap` bytes long; `ret` is a valid out-pointer.
        let r = unsafe {
            GetTokenInformation(
                tok.raw(),
                TokenUser,
                Some(buf.as_mut_ptr().cast()),
                cap,
                &raw mut ret,
            )
        };
        if r.is_ok() {
            break;
        }
        buf.resize((ret as usize).div_ceil(8).max(buf.len() + 1), 0);
    }
    // SAFETY: GetTokenInformation wrote a TOKEN_USER at the 8-aligned start of `buf`.
    let tu: TOKEN_USER = unsafe { std::ptr::read(buf.as_ptr().cast()) };
    let mut s = PWSTR::null();
    // SAFETY: `tu.User.Sid` points into `buf` (alive); `s` receives a LocalAlloc'ed string.
    unsafe { ConvertSidToStringSidW(tu.User.Sid, &raw mut s) }
        .map_err(|e| os("ConvertSidToStringSidW", &e))?;
    Ok(take_local_string(s))
}

/// String SID (`S-1-5-21-…`) of the user this process runs as.
///
/// # Errors
/// If the token cannot be read.
pub fn current_user_sid() -> Result<String> {
    // SAFETY: the pseudo-handle needs no closing.
    token_sid(unsafe { GetCurrentProcess() })
}

/// `N` bytes from the system CSPRNG.
///
/// # Errors
/// If `BCryptGenRandom` fails.
pub fn random_bytes<const N: usize>() -> Result<[u8; N]> {
    let mut b = [0u8; N];
    // SAFETY: `b` is a valid writable buffer; the system-preferred RNG needs no handle.
    let st = unsafe { BCryptGenRandom(None, &mut b, BCRYPT_USE_SYSTEM_PREFERRED_RNG) };
    if st.is_ok() {
        Ok(b)
    } else {
        Err(CoreError::Os {
            api: "BCryptGenRandom",
            code: st.0,
        })
    }
}

/// A local, single-instance, byte-mode named pipe that only `allowed_sid` may open.
pub struct PipeServer {
    h: OwnedHandle,
}

impl PipeServer {
    /// Creates `\\.\pipe\<name>` with DACL `D:P(A;;GA;;;<allowed_sid>)`, rejecting remote
    /// clients and a second instance.
    ///
    /// # Errors
    /// If the pipe exists already or cannot be created.
    pub fn create(name: &str, allowed_sid: &str) -> Result<Self> {
        if !allowed_sid.starts_with("S-1-") || allowed_sid.contains([')', '(', ';']) {
            return Err(CoreError::Unsupported);
        }
        let sddl = HSTRING::from(format!("D:P(A;;GA;;;{allowed_sid})"));
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: `sddl` outlives the call; `psd` receives a LocalAlloc'ed descriptor.
        unsafe {
            ConvertStringSecurityDescriptorToSecurityDescriptorW(
                &sddl,
                SDDL_REVISION_1,
                &raw mut psd,
                None,
            )
        }
        .map_err(|e| os("ConvertStringSecurityDescriptorToSecurityDescriptorW", &e))?;
        let sa = SECURITY_ATTRIBUTES {
            nLength: u32::try_from(std::mem::size_of::<SECURITY_ATTRIBUTES>()).unwrap_or(0),
            lpSecurityDescriptor: psd.0,
            bInheritHandle: false.into(),
        };
        let path = HSTRING::from(format!(r"\\.\pipe\{name}"));
        // SAFETY: `path` and `sa` (with its descriptor) are alive for the call.
        let h = unsafe {
            CreateNamedPipeW(
                &path,
                PIPE_ACCESS_DUPLEX | FILE_FLAG_FIRST_PIPE_INSTANCE,
                PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
                1,
                4096,
                4096,
                0,
                Some(&raw const sa),
            )
        };
        let err = windows::core::Error::from_thread();
        // SAFETY: freeing the descriptor allocated above; the pipe copied it.
        unsafe {
            LocalFree(Some(HLOCAL(psd.0)));
        }
        if h.is_invalid() {
            return Err(os("CreateNamedPipeW", &err));
        }
        Ok(Self {
            h: OwnedHandle::new(h),
        })
    }

    /// The pipe's DACL as SDDL (for verification).
    ///
    /// # Errors
    /// If the security descriptor cannot be read.
    pub fn dacl_sddl(&self) -> Result<String> {
        let mut psd = PSECURITY_DESCRIPTOR::default();
        // SAFETY: valid handle and out-pointers; the descriptor is LocalAlloc'ed.
        let r = unsafe {
            GetSecurityInfo(
                self.h.raw(),
                SE_KERNEL_OBJECT,
                DACL_SECURITY_INFORMATION,
                None,
                None,
                None,
                None,
                Some(&raw mut psd),
            )
        };
        if r.is_err() {
            return Err(CoreError::Os {
                api: "GetSecurityInfo",
                code: i32::try_from(r.0).unwrap_or(i32::MAX),
            });
        }
        let mut s = PWSTR::null();
        // SAFETY: `psd` is the descriptor returned above; `s` receives a LocalAlloc'ed string.
        let conv = unsafe {
            ConvertSecurityDescriptorToStringSecurityDescriptorW(
                psd,
                SDDL_REVISION_1,
                DACL_SECURITY_INFORMATION,
                &raw mut s,
                None,
            )
        };
        // SAFETY: freeing the descriptor GetSecurityInfo allocated.
        unsafe {
            LocalFree(Some(HLOCAL(psd.0)));
        }
        conv.map_err(|e| os("ConvertSecurityDescriptorToStringSecurityDescriptorW", &e))?;
        Ok(take_local_string(s))
    }

    /// Waits for a client.
    ///
    /// # Errors
    /// If the connection fails.
    pub fn accept(&self) -> Result<PipeConn<'_>> {
        // SAFETY: a live pipe handle; synchronous connect.
        match unsafe { ConnectNamedPipe(self.h.raw(), None) } {
            Ok(()) => Ok(PipeConn { server: self }),
            Err(e) if e.code().0 & 0xFFFF == ERROR_PIPE_CONNECTED => Ok(PipeConn { server: self }),
            Err(e) => Err(os("ConnectNamedPipe", &e)),
        }
    }
}

/// One connected client; disconnects on drop so the pipe can accept again.
pub struct PipeConn<'a> {
    server: &'a PipeServer,
}

impl PipeConn<'_> {
    /// SID of the process on the other end.
    ///
    /// # Errors
    /// If the client process or its token cannot be read.
    pub fn client_sid(&self) -> Result<String> {
        let mut pid = 0u32;
        // SAFETY: a connected pipe handle and a valid out-pointer.
        unsafe { GetNamedPipeClientProcessId(self.server.h.raw(), &raw mut pid) }
            .map_err(|e| os("GetNamedPipeClientProcessId", &e))?;
        // SAFETY: plain call; the handle is owned below.
        let h = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, false, pid) }
            .map_err(|e| os("OpenProcess", &e))?;
        let h = OwnedHandle::new(h);
        token_sid(h.raw())
    }
}

impl std::io::Read for PipeConn<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        // SAFETY: `buf` is writable for its length; synchronous read.
        match unsafe { ReadFile(self.server.h.raw(), Some(buf), Some(&raw mut n), None) } {
            Ok(()) => Ok(n as usize),
            Err(e) if e.code().0 & 0xFFFF == ERROR_BROKEN_PIPE => Ok(0),
            Err(e) => Err(std::io::Error::other(e)),
        }
    }
}

impl std::io::Write for PipeConn<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let mut n = 0u32;
        // SAFETY: `buf` is readable for its length; synchronous write.
        unsafe { WriteFile(self.server.h.raw(), Some(buf), Some(&raw mut n), None) }
            .map_err(std::io::Error::other)?;
        Ok(n as usize)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        // SAFETY: a connected pipe handle.
        unsafe { FlushFileBuffers(self.server.h.raw()) }.map_err(std::io::Error::other)
    }
}

impl Drop for PipeConn<'_> {
    fn drop(&mut self) {
        // SAFETY: a pipe handle; ends this client's session so the pipe can accept again.
        unsafe {
            let _ = FlushFileBuffers(self.server.h.raw());
            let _ = DisconnectNamedPipe(self.server.h.raw());
        }
    }
}

/// Starts `exe args` elevated (`ShellExecuteExW` verb `runas`: one UAC prompt).
///
/// # Errors
/// If the user declines (`ERROR_CANCELLED`) or the launch fails.
pub fn run_elevated(exe: &Path, args: &str) -> Result<()> {
    let file = HSTRING::from(exe.as_os_str());
    let params = HSTRING::from(args);
    let mut info = SHELLEXECUTEINFOW {
        cbSize: u32::try_from(std::mem::size_of::<SHELLEXECUTEINFOW>()).unwrap_or(0),
        fMask: SEE_MASK_NOASYNC,
        lpVerb: w!("runas"),
        lpFile: windows::core::PCWSTR(file.as_ptr()),
        lpParameters: windows::core::PCWSTR(params.as_ptr()),
        nShow: SW_HIDE.0,
        ..Default::default()
    };
    // SAFETY: `info` and the strings it points to are alive for the call.
    unsafe { ShellExecuteExW(&raw mut info) }.map_err(|e| os("ShellExecuteExW", &e))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Read as _, Write as _};

    fn unique(tag: &str) -> String {
        let r: [u8; 8] = random_bytes().unwrap();
        format!(
            "atropos-test-{tag}-{}",
            r.iter().fold(String::new(), |mut s, b| {
                use std::fmt::Write as _;
                let _ = write!(s, "{b:02x}");
                s
            })
        )
    }

    #[test]
    fn current_user_sid_is_a_string_sid() {
        let sid = current_user_sid().unwrap();
        assert!(sid.starts_with("S-1-"), "{sid}");
    }

    #[test]
    fn random_bytes_are_random() {
        let a: [u8; 32] = random_bytes().unwrap();
        let b: [u8; 32] = random_bytes().unwrap();
        assert_ne!(a, b);
    }

    #[test]
    fn dacl_grants_only_the_given_sid() {
        let me = current_user_sid().unwrap();
        let p = PipeServer::create(&unique("dacl"), &me).unwrap();
        let sddl = p.dacl_sddl().unwrap();
        assert!(sddl.starts_with("D:P"), "protected DACL: {sddl}");
        assert_eq!(
            sddl.matches("(A;").count(),
            1,
            "exactly one allow ACE: {sddl}"
        );
        assert!(sddl.contains(&me), "{sddl}");
    }

    #[test]
    fn the_allowed_user_connects_and_is_identified() {
        let me = current_user_sid().unwrap();
        let name = unique("echo");
        let server = PipeServer::create(&name, &me).unwrap();
        let path = format!(r"\\.\pipe\{name}");
        let client = std::thread::spawn(move || {
            let mut f = std::fs::OpenOptions::new()
                .read(true)
                .write(true)
                .open(path)
                .unwrap();
            f.write_all(b"ping").unwrap();
            let mut back = [0u8; 4];
            f.read_exact(&mut back).unwrap();
            back
        });
        let mut conn = server.accept().unwrap();
        assert_eq!(conn.client_sid().unwrap(), me);
        let mut buf = [0u8; 4];
        conn.read_exact(&mut buf).unwrap();
        conn.write_all(b"pong").unwrap();
        assert_eq!(&client.join().unwrap(), b"pong");
        assert_eq!(&buf, b"ping");
    }

    #[test]
    fn a_wrong_sid_cannot_even_open_the_pipe() {
        // Granted to SYSTEM only: this (non-SYSTEM) test process must be refused by the kernel.
        let name = unique("wrongsid");
        let _server = PipeServer::create(&name, "S-1-5-18").unwrap();
        let err = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(format!(r"\\.\pipe\{name}"))
            .unwrap_err();
        assert_eq!(err.raw_os_error(), Some(5), "access denied: {err}");
    }

    #[test]
    fn a_second_instance_is_refused() {
        let me = current_user_sid().unwrap();
        let name = unique("single");
        let _a = PipeServer::create(&name, &me).unwrap();
        assert!(PipeServer::create(&name, &me).is_err());
    }
}
