//! STUB (stage c): send keys over ADB (`adb connect <host>:5555`, then
//! `adb shell input keyevent <KEYCODE_…>`). Needs developer mode on the TV.
//!
//! Testing: only the stub's error path is tested.

use anyhow::{Result, anyhow};
use castdesk_core::not_implemented;

use super::Key;

pub async fn send_key(_target: &str, _key: Key) -> Result<()> {
    Err(anyhow!(not_implemented("ADB remote control", "c")))
}
