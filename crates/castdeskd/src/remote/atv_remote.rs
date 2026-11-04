//! STUB (stage c): Android TV Remote protocol v2.
//!
//! Plan: TLS with a self-signed client certificate; pairing on port 6467
//! (protobuf messages, the user types the 6-hex-digit code shown on the TV),
//! then remote commands on port 6466. Pairing secrets are kept under
//! `~/.local/state/castdesk/`.
//!
//! Testing: only the stub's error path is tested. Never run against a TV.

use anyhow::{Result, anyhow};
use castdesk_core::not_implemented;

use super::Key;

/// Pair with a TV using the code it displays.
pub async fn pair(_host: &str, _code: &str) -> Result<()> {
    Err(anyhow!(not_implemented("Android TV Remote pairing", "c")))
}

pub async fn send_key(_host: &str, _key: Key) -> Result<()> {
    Err(anyhow!(not_implemented("Android TV Remote", "c")))
}
