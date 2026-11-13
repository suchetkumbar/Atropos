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
}

/// Core result alias.
pub type Result<T> = std::result::Result<T, CoreError>;
