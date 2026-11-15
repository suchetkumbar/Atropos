//! Atropos core: data model, `Platform` trait, collector, kill engine. No UI code.
//!
//! All `unsafe` code lives in `platform::windows`; everywhere else it is denied.
#![deny(unsafe_code)]
#![deny(unsafe_op_in_unsafe_fn)]

pub mod collector;
pub mod config;
pub mod context;
pub mod cpu;
pub mod engine;
pub mod error;
pub mod filter;
pub mod gpu;
pub mod journal;
pub mod labels;
pub mod model;
pub mod paths;
pub mod pin;
pub mod platform;
pub mod ports;
pub mod projects;
pub mod ring;
pub mod secrets;
pub mod snapshot;
pub mod statics;
pub mod tree;
pub mod treemap;

pub use error::{CoreError, Result};
pub use model::{Field, ProcKey};
