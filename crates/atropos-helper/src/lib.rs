//! The elevation helper (spec §4.7). `Atropos.exe --elevated-helper` runs [`run`] after a UAC
//! prompt; the GUI talks to it through [`client`]. Nothing persists after it exits.
#![deny(unsafe_code)]

#[cfg(windows)]
pub mod client;
#[cfg(windows)]
pub mod host;
pub mod protocol;
#[cfg(windows)]
pub mod server;
