//! STUB (stage c): soft remote — control the TV / receiver from the laptop.
//!
//! Pieces, tried in this order per receiver:
//! * [`atv_remote`]: Android TV Remote v2 (the protocol Google's TV remote app
//!   uses). No Rust library exists, so it is implemented here.
//! * [`adb`]: `adb shell input keyevent …` fallback for devices with
//!   developer mode on.
//! * [`cec`]: HDMI-CEC via `cec-client`, optional.
//! * [`control_ws`]: CastDesk's own WebSocket protocol for the custom Media3
//!   receiver app (see `docs/control-protocol.md`).
//!
//! Testing: only the stubs' error paths are tested. Nothing here talks to a
//! real device. The API route `/api/v1/control` answers 501 Not Implemented.
#![allow(dead_code)] // not wired until stage (c)

pub mod adb;
pub mod atv_remote;
pub mod cec;
pub mod control_ws;

/// Remote-control keys every transport should map.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Key {
    Up,
    Down,
    Left,
    Right,
    Select,
    Back,
    Home,
    PlayPause,
    VolumeUp,
    VolumeDown,
    Mute,
    Power,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn stubs_report_not_implemented() {
        let errs = [
            adb::send_key("192.0.2.1:5555", Key::Home).await.unwrap_err(),
            atv_remote::pair("192.0.2.1", "123456").await.unwrap_err(),
            atv_remote::send_key("192.0.2.1", Key::Home).await.unwrap_err(),
            cec::send_key(Key::Power).await.unwrap_err(),
        ];
        for e in errs {
            assert!(e.to_string().contains("not implemented"), "{e}");
        }
    }
}
