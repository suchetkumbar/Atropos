//! The elevation helper (spec §4.7). `Atropos.exe --elevated-helper` runs [`run`] after a UAC
//! prompt; the GUI talks to it through [`client`]. Nothing persists after it exits.
#![deny(unsafe_code)]

pub mod client;
pub mod host;
pub mod protocol;
pub mod server;
