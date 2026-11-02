//! GNOME: `org.gnome.Mutter.ScreenCast.Session.RecordVirtual`.
//!
//! Mutter creates a real virtual monitor for as long as the stream exists and
//! sizes it from the video format the consumer negotiates — so the pipeline
//! asks pipewiresrc for exactly width×height. No portal dialog is involved.
//! The monitor disappears when we call Stop or our D-Bus connection drops.

use std::collections::HashMap;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use castdesk_core::DisplaySpec;
use futures_util::StreamExt;
use zbus::zvariant::{OwnedObjectPath, Value};

use super::CaptureSource;

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast",
    default_service = "org.gnome.Mutter.ScreenCast",
    default_path = "/org/gnome/Mutter/ScreenCast"
)]
trait ScreenCast {
    fn create_session(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;
    #[zbus(property)]
    fn version(&self) -> zbus::Result<i32>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Session",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
trait ScreenCastSession {
    fn start(&self) -> zbus::Result<()>;
    fn stop(&self) -> zbus::Result<()>;
    fn record_virtual(&self, properties: HashMap<&str, Value<'_>>) -> zbus::Result<OwnedObjectPath>;
    #[zbus(signal)]
    fn closed(&self) -> zbus::Result<()>;
}

#[zbus::proxy(
    interface = "org.gnome.Mutter.ScreenCast.Stream",
    default_service = "org.gnome.Mutter.ScreenCast"
)]
trait ScreenCastStream {
    #[zbus(signal, name = "PipeWireStreamAdded")]
    fn pipewire_stream_added(&self, node_id: u32) -> zbus::Result<()>;
}

/// Cursor modes from the Mutter API.
const CURSOR_EMBEDDED: u32 = 1;

pub struct MutterDisplay {
    pub source: CaptureSource,
    // Keep the connection alive: Mutter closes the session when it drops.
    _conn: zbus::Connection,
    session: ScreenCastSessionProxy<'static>,
}

impl MutterDisplay {
    pub async fn create(spec: &DisplaySpec) -> Result<Self> {
        let conn = zbus::Connection::session().await.context("connecting to session bus")?;
        let sc = ScreenCastProxy::new(&conn).await?;
        let version = sc.version().await.context(
            "org.gnome.Mutter.ScreenCast not available (not GNOME, or Mutter too old)",
        )?;
        tracing::debug!(version, "Mutter ScreenCast API");

        let session_path = sc.create_session(HashMap::new()).await.map_err(|e| {
            anyhow!("CreateSession failed: {e} (if AccessDenied, this GNOME restricts the private API; set backend = \"portal\")")
        })?;
        let session = ScreenCastSessionProxy::builder(&conn)
            .path(session_path)?
            .build()
            .await?;

        let mut props: HashMap<&str, Value<'_>> = HashMap::new();
        props.insert("cursor-mode", Value::U32(CURSOR_EMBEDDED));
        // Treat as a normal (non-mirrored) extended monitor.
        props.insert("is-platform", Value::Bool(false));
        let stream_path = session.record_virtual(props).await.context("RecordVirtual")?;
        let stream = ScreenCastStreamProxy::builder(&conn)
            .path(stream_path)?
            .build()
            .await?;

        let mut added = stream.receive_pipewire_stream_added().await?;
        session.start().await.context("Session.Start")?;
        let sig = tokio::time::timeout(Duration::from_secs(10), added.next())
            .await
            .map_err(|_| anyhow!("timed out waiting for PipeWireStreamAdded"))?
            .ok_or_else(|| anyhow!("signal stream ended"))?;
        let node_id = sig.args()?.node_id;
        tracing::info!(display = %spec.name, node_id, "Mutter virtual monitor stream ready");

        Ok(Self {
            source: CaptureSource::PipeWire {
                fd: None,
                node_id,
                drives_size: true,
            },
            _conn: conn,
            session,
        })
    }

    pub async fn stop(self) -> Result<()> {
        self.session.stop().await.context("Session.Stop")?;
        Ok(())
    }
}
