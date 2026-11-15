//! Core error type. A single unreadable field never becomes an error; see [`crate::Field`].

/// Errors returned by core operations.
#[derive(Debug, thiserror::Error, Clone, PartialEq, Eq)]
pub enum CoreError {
    /// An OS call failed. `code` is the Win32 error / NTSTATUS as reported.
    #[error("{api} failed (os error {code})")]
    Os { api: &'static str, code: i32 },
    /// The operation is not supported on this platform or OS version.
    #[error("operation not supported on this platform")]
    Unsupported,
    /// The file or folder asked about does not exist.
    #[error("the path does not exist")]
    NotFound,
    /// The OS did not answer in time (e.g. a file lookup on an unreachable network share).
    #[error("no answer in time")]
    TimedOut,
}

/// Core result alias.
pub type Result<T> = std::result::Result<T, CoreError>;
