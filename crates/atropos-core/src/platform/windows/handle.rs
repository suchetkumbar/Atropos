//! RAII handle: closes on drop (spec §3.4).

use windows::Win32::Foundation::{CloseHandle, HANDLE};

/// An owned kernel handle.
#[derive(Debug)]
pub(crate) struct OwnedHandle(HANDLE);

impl OwnedHandle {
    /// Takes ownership of `h`, which must be a valid handle the caller will not close.
    pub(crate) fn new(h: HANDLE) -> Self {
        Self(h)
    }

    pub(crate) fn raw(&self) -> HANDLE {
        self.0
    }
}

impl Drop for OwnedHandle {
    fn drop(&mut self) {
        // SAFETY: we own the handle (see `new`) and close it exactly once.
        let _ = unsafe { CloseHandle(self.0) };
    }
}
