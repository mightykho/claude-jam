//! Claude Jam — testable core logic.
//!
//! The binary (`src/main.rs`) parses CLI arguments and owns the TUI; everything
//! exit-free and UI-free lives in this lib so it can be tested in isolation and
//! reused by downstream consumers.

pub mod beads;
pub mod db;
pub mod hook;
pub mod models;
pub mod setup;
pub mod time;
pub mod tmux;
