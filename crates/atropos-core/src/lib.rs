//! Atropos core: data model, `Platform` trait, collector, kill engine. No UI code.
//!
//! All `unsafe` code lives in the OS backends under `platform` (`windows`, `macos`, and the
//! small `unix_time`); everywhere else it is denied.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod collector;
pub mod config;
pub mod context;
pub mod cpu;
pub mod engine;
pub mod error;
pub mod export;
pub mod filter;
pub mod gpu;
pub mod history;
pub mod journal;
pub mod labels;
pub mod locks;
pub mod macos_text;
pub mod model;
pub mod paths;
pub mod pin;
pub mod platform;
pub mod ports;
pub mod procfs;
pub mod projects;
pub mod ring;
pub mod secrets;
pub mod snapshot;
pub mod statics;
pub mod time;
pub mod tree;
pub mod treemap;
pub mod tune;

pub use error::{CoreError, Result};
pub use model::{Field, ProcKey};
