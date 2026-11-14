//! Windows backend. This is the only module where `unsafe` is allowed; every block carries a `// SAFETY:` note.

mod alloc;
mod ctrl_c;
mod dialog;
pub mod fixture_support;
mod graceful;
mod handle;
mod job;
mod net;
mod peb;
mod pin;
pub mod pipe;
mod process;
mod scan;
mod services;
mod statics;
mod time;

pub use alloc::CountingAlloc;
pub use ctrl_c::{ctrl_c_helper_main, send_ctrl_c};
pub use dialog::fatal_message;
pub(crate) use handle::OwnedHandle;
pub use process::{current_process_key, now_filetime};
pub use scan::WindowsPlatform;
pub use time::{LocalTime, local_time};

use windows::Win32::Foundation::FILETIME;

/// Packs a FILETIME into 100 ns ticks since 1601-01-01 UTC.
#[inline]
pub(crate) fn filetime_to_u64(ft: FILETIME) -> u64 {
    (u64::from(ft.dwHighDateTime) << 32) | u64::from(ft.dwLowDateTime)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn filetime_packs_high_and_low() {
        let ft = FILETIME {
            dwLowDateTime: 0x0000_0001,
            dwHighDateTime: 0x0000_0002,
        };
        assert_eq!(filetime_to_u64(ft), 0x0000_0002_0000_0001);
    }
}
