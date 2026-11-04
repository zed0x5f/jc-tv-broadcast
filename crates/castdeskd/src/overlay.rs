//! STUB (stage b): latency-test overlay.
//!
//! Plan: burn the sender's wall clock (ms) into the frame before encoding, so
//! glass-to-glass latency can be read by photographing the laptop screen next
//! to a receiver. Likely `clockoverlay time-format=%H:%M:%S.%f` or a
//! `timeoverlay` driven by the pipeline clock.
//!
//! Testing: only the stub's error path is tested. Not wired into the pipeline;
//! `DisplaySpec::validate` rejects `latency_overlay = true`.
#![allow(dead_code)] // not wired until stage (b)

use anyhow::{Result, anyhow};
use castdesk_core::not_implemented;

/// gst-launch fragment inserted between capture and the encoder.
pub fn description() -> Result<String> {
    Err(anyhow!(not_implemented("the latency overlay", "b")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_reports_not_implemented() {
        assert!(super::description().unwrap_err().to_string().contains("not implemented"));
    }
}
