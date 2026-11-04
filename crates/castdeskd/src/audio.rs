//! STUB (stage b): desktop audio.
//!
//! Plan: capture the default sink's monitor with `pipewiresrc` (stream
//! property `stream.capture.sink=true`), encode once (AAC for RTSP/MPEG-TS,
//! Opus where the receiver supports it) and mux it into every destination
//! next to the video.
//!
//! Testing: only the stub's error path is tested. Not wired into the pipeline;
//! `DisplaySpec::validate` rejects `audio = true`.
#![allow(dead_code)] // not wired until stage (b)

use anyhow::{Result, anyhow};
use castdesk_core::not_implemented;

/// gst-launch fragment for the audio capture + encode branch.
pub fn source_description() -> Result<String> {
    Err(anyhow!(not_implemented("audio", "b")))
}

#[cfg(test)]
mod tests {
    #[test]
    fn stub_reports_not_implemented() {
        assert!(super::source_description().unwrap_err().to_string().contains("not implemented"));
    }
}
