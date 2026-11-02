//! Shared types for CastDesk: configuration, the control-API data model and an
//! HTTP client for the daemon's localhost API. Used by `castdeskd` and every
//! front end (TUI now, GTK widget later).

pub mod api;
pub mod client;
pub mod config;
pub mod paths;

pub use api::*;
pub use config::Config;
