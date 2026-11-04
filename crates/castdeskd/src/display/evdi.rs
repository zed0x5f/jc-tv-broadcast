//! STUB (stage b): DisplayLink `evdi` backend.
//!
//! Plan: load the evdi kernel module (DKMS, needs root, and MOK signing with
//! Secure Boot), open `/dev/dri/cardN` for a new evdi device, attach an EDID
//! for the requested mode, and grab frames from the device's buffers. That
//! gives a real DRM connector, for desktops where none of the other backends
//! work.
//!
//! Testing: none. Nothing here is implemented. `DisplaySpec::validate` rejects
//! `backend = "evdi"` before this is reached; `create` errors as a backstop.

use anyhow::{Result, anyhow};
use castdesk_core::{DisplaySpec, not_implemented};

use super::DisplayHandle;

pub async fn create(_spec: &DisplaySpec) -> Result<DisplayHandle> {
    Err(anyhow!(not_implemented("the evdi backend", "b")))
}
