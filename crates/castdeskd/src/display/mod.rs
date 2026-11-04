//! Virtual display backends. Each creates an extra monitor using existing
//! compositor/X mechanisms (no custom kernel code) and returns a capture source.
//!
//! Testing: only the `test` backend has run (self-test). `auto` fallback across
//! real backends has never been exercised.

pub mod evdi; // stub, stage (b)
pub mod mutter;
pub mod portal;
pub mod wlroots;
pub mod x11;

use std::os::fd::{AsRawFd, OwnedFd};

use anyhow::{Result, anyhow};
use castdesk_core::{BackendKind, DisplaySpec, SessionInfo};

/// What the pipeline should read from.
#[derive(Debug)]
pub enum CaptureSource {
    PipeWire {
        /// Portal-provided PipeWire remote; `None` = the session's default remote.
        fd: Option<OwnedFd>,
        node_id: u32,
        /// The compositor sizes the virtual monitor from the caps we request
        /// (Mutter virtual streams), so put width/height right after pipewiresrc.
        drives_size: bool,
    },
    X11 {
        display: String,
        x: i32,
        y: i32,
        width: u32,
        height: u32,
    },
    Test,
}

impl CaptureSource {
    pub fn raw_fd(&self) -> Option<i32> {
        match self {
            CaptureSource::PipeWire { fd: Some(fd), .. } => Some(fd.as_raw_fd()),
            _ => None,
        }
    }
}

pub enum DisplayHandle {
    Mutter(mutter::MutterDisplay),
    Portal(portal::PortalDisplay),
    Wlroots(wlroots::WlrootsDisplay),
    X11(x11::X11Display),
    Test(CaptureSource),
}

impl DisplayHandle {
    pub fn source(&self) -> &CaptureSource {
        match self {
            DisplayHandle::Mutter(d) => &d.source,
            DisplayHandle::Portal(d) => &d.source,
            DisplayHandle::Wlroots(d) => &d.portal.source,
            DisplayHandle::X11(d) => &d.source,
            DisplayHandle::Test(s) => s,
        }
    }

    pub fn output_name(&self) -> Option<String> {
        match self {
            DisplayHandle::Wlroots(d) => Some(d.output.clone()),
            DisplayHandle::X11(d) => Some(d.monitor.clone()),
            _ => None,
        }
    }

    /// Tear down the virtual monitor.
    pub async fn remove(self) {
        let res = match self {
            DisplayHandle::Mutter(d) => d.stop().await,
            DisplayHandle::Portal(d) => d.stop().await,
            DisplayHandle::Wlroots(d) => d.remove().await,
            DisplayHandle::X11(d) => d.remove().await,
            DisplayHandle::Test(_) => Ok(()),
        };
        if let Err(e) = res {
            tracing::warn!("removing virtual display: {e:#}");
        }
    }
}

/// Create a virtual display, resolving `auto` against the session.
pub async fn create(
    spec: &DisplaySpec,
    session: &SessionInfo,
) -> Result<(BackendKind, DisplayHandle)> {
    let order = match spec.backend {
        BackendKind::Auto => session.backend_order.clone(),
        b => vec![b],
    };
    if order.is_empty() {
        return Err(anyhow!(
            "no virtual-display backend for session {}/{} — run castdeskd inside your graphical session, or use backend = \"test\"",
            session.kind,
            session.desktop
        ));
    }
    let mut errors = Vec::new();
    for kind in order {
        let res = match kind {
            BackendKind::Mutter => mutter::MutterDisplay::create(spec).await.map(DisplayHandle::Mutter),
            BackendKind::Portal => portal::PortalDisplay::create_virtual(spec).await.map(DisplayHandle::Portal),
            BackendKind::Wlroots => wlroots::WlrootsDisplay::create(spec, session).await.map(DisplayHandle::Wlroots),
            BackendKind::X11 => x11::X11Display::create(spec).await.map(DisplayHandle::X11),
            BackendKind::Test => Ok(DisplayHandle::Test(CaptureSource::Test)),
            BackendKind::Evdi => evdi::create(spec).await,
            BackendKind::Auto => unreachable!(),
        };
        match res {
            Ok(h) => {
                tracing::info!(display = %spec.name, backend = kind.label(), "virtual display created");
                return Ok((kind, h));
            }
            Err(e) => {
                tracing::warn!(display = %spec.name, backend = kind.label(), "backend failed: {e:#}");
                errors.push(format!("{}: {e:#}", kind.label()));
            }
        }
    }
    Err(anyhow!("all backends failed — {}", errors.join("; ")))
}
