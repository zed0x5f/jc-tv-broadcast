//! Shared types for CastDesk: configuration, the control-API data model and an
//! HTTP client for the daemon's localhost API. Used by `castdeskd` and every
//! front end (currently only the TUI).
//!
//! Testing: config load/save/validation is unit-tested (`config.rs`).
//! The HTTP client (`client.rs`) has no automated tests. The self-test script
//! talks to the daemon with curl, not this client.

pub mod api;
pub mod client;
pub mod config;
pub mod paths;

pub use api::*;
pub use config::Config;
