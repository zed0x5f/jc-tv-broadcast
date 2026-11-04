//! STUB (stage b): extra outputs on the encoder `tee`, next to the RTSP
//! publish branch.
//!
//! Plan: each `Destination` becomes one `t. ! queue ! … ! sink` branch that is
//! linked/unlinked on a running pipeline (pad probes on the tee's request pad),
//! so toggling an output never restarts the encoder.
//! * SRT: `mpegtsmux ! srtsink mode=listener uri=srt://:<port>`
//! * UDP: `mpegtsmux ! rtpmp2tpay? ! udpsink host=… port=… ttl-mc=…`
//!   (multicast needs IGMP snooping notes in the README.)
//!
//! Testing: only the stub's error path is tested. Nothing here is wired into
//! the pipeline; `DisplaySpec::validate` rejects any configured destination.
#![allow(dead_code)] // not wired until stage (b)

use anyhow::{Result, anyhow};
use castdesk_core::{Destination, not_implemented};

/// gst-launch fragment for one destination branch, starting after `t.`.
pub fn branch_description(dest: &Destination) -> Result<String> {
    let what = match dest {
        Destination::Srt { .. } => "the SRT destination",
        Destination::Udp { .. } => "the UDP destination",
    };
    Err(anyhow!(not_implemented(what, "b")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stub_reports_not_implemented() {
        let d = Destination::Srt { port: 9000, latency_ms: 120, passphrase: None };
        assert!(branch_description(&d).unwrap_err().to_string().contains("not implemented"));
    }
}
