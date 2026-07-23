//! rustranger library crate.
//!
//! The binary (`src/main.rs`) is a thin shell over these modules: terminal
//! setup, the event loop, and key dispatch. Exposing the modules as a library
//! lets benchmarks (`benches/`) and integration tests drive the internals
//! (directory loading, sorting, filtering, preview parsing) directly.

// Several enums expose `fn from_str(&str) -> Option<Self>` keyword parsers.
// These intentionally return Option (not Result like `std::str::FromStr`), so
// the trait-confusion lint is a false positive here. (Surfaced once the crate
// became a library; harmless as inherent helpers.)
#![allow(clippy::should_implement_trait)]

pub mod app;
pub mod clipboard;
pub mod config;
pub mod console;
pub mod fs;
pub mod image;
pub mod open;
pub mod ops;
pub mod preview;
pub mod screen;
pub mod state;
pub mod tab;
pub mod theme;
pub mod ui;
pub mod util;
