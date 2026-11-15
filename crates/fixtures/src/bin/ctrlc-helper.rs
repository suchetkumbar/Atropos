//! Test copy of the console Ctrl+C helper (`atr.exe __ctrl-c <pid> <create_time>`): Cargo does
//! not build `atr.exe` for this crate's tests, so the fixtures carry the same entry point.

#[cfg(windows)]
fn main() -> std::process::ExitCode {
    atropos_core::platform::windows::ctrl_c_helper_main(std::env::args().skip(1))
}

/// Windows only.
#[cfg(not(windows))]
fn main() {}
