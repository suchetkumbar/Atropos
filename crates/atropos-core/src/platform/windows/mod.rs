//! Windows backend. This is the only module where `unsafe` is allowed; every block carries a `// SAFETY:` note.

mod dialog;
mod process;

pub use dialog::fatal_message;
pub use process::{current_process_key, now_filetime};

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
