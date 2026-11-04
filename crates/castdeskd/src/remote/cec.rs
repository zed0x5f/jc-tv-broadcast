//! STUB (stage c, optional): HDMI-CEC through `cec-client` (libcec), for
//! laptops/adapters that expose a CEC device.
//!
//! Testing: only the stub's error path is tested.

use anyhow::{Result, anyhow};
use castdesk_core::not_implemented;

use super::Key;

pub async fn send_key(_key: Key) -> Result<()> {
    Err(anyhow!(not_implemented("HDMI-CEC control", "c")))
}
