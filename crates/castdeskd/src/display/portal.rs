//! xdg-desktop-portal ScreenCast.
//!
//! * `create_virtual` asks for source type VIRTUAL: the compositor creates a new
//!   virtual monitor (KDE Plasma 6, GNOME 44+ via xdg-desktop-portal-gnome).
//! * `capture_monitor` asks for a MONITOR (used for wlroots headless outputs,
//!   which we create ourselves; the user picks it in the portal's chooser once).
//!
//! The first run shows a permission dialog; we save the restore token per
//! display so later starts are silent (PersistMode::ExplicitlyRevoked).

use std::collections::HashMap;
use std::path::PathBuf;

use anyhow::{Context, Result, anyhow};
use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::screencast::{CursorMode, Screencast, SourceType};
use castdesk_core::{DisplaySpec, paths};

use super::CaptureSource;

pub struct PortalDisplay {
    pub source: CaptureSource,
    session: Session<'static, Screencast<'static>>,
    _proxy: Screencast<'static>,
}

fn tokens_file() -> PathBuf {
    paths::state_dir().join("portal-tokens.json")
}

fn load_token(key: &str) -> Option<String> {
    let text = std::fs::read_to_string(tokens_file()).ok()?;
    let map: HashMap<String, String> = serde_json::from_str(&text).ok()?;
    map.get(key).cloned()
}

fn save_token(key: &str, token: &str) {
    let path = tokens_file();
    let mut map: HashMap<String, String> = std::fs::read_to_string(&path)
        .ok()
        .and_then(|t| serde_json::from_str(&t).ok())
        .unwrap_or_default();
    map.insert(key.to_string(), token.to_string());
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Err(e) = std::fs::write(&path, serde_json::to_string_pretty(&map).unwrap_or_default()) {
        tracing::warn!("saving portal restore token: {e}");
    }
}

impl PortalDisplay {
    pub async fn create_virtual(spec: &DisplaySpec) -> Result<Self> {
        Self::open(spec, SourceType::Virtual, &format!("virtual:{}", spec.name)).await
    }

    pub async fn capture_monitor(spec: &DisplaySpec, output: &str) -> Result<Self> {
        Self::open(spec, SourceType::Monitor, &format!("monitor:{output}")).await
    }

    async fn open(spec: &DisplaySpec, kind: SourceType, token_key: &str) -> Result<Self> {
        let proxy = Screencast::new()
            .await
            .context("xdg-desktop-portal ScreenCast unavailable (is xdg-desktop-portal + a backend installed?)")?;
        let available = proxy.available_source_types().await.unwrap_or_default();
        if !available.contains(kind) {
            return Err(anyhow!(
                "portal does not offer {kind:?} sources (offers {available:?}); virtual monitors need Plasma 6 or GNOME 44+"
            ));
        }
        let session = proxy.create_session().await.context("CreateSession")?;
        let token = load_token(token_key);
        proxy
            .select_sources(
                &session,
                CursorMode::Embedded,
                kind.into(),
                false,
                token.as_deref(),
                PersistMode::ExplicitlyRevoked,
            )
            .await
            .context("SelectSources")?;
        tracing::info!(display = %spec.name, ?kind, restored = token.is_some(), "waiting for portal Start (a dialog may appear)");
        let streams = proxy
            .start(&session, None)
            .await
            .context("Start")?
            .response()
            .context("portal request was cancelled or denied")?;
        if let Some(t) = streams.restore_token() {
            save_token(token_key, t);
        }
        let stream = streams
            .streams()
            .first()
            .ok_or_else(|| anyhow!("portal returned no streams"))?;
        let node_id = stream.pipe_wire_node_id();
        let fd = proxy
            .open_pipe_wire_remote(&session)
            .await
            .context("OpenPipeWireRemote")?;
        tracing::info!(display = %spec.name, node_id, size = ?stream.size(), "portal stream ready");
        Ok(Self {
            source: CaptureSource::PipeWire {
                fd: Some(fd),
                node_id,
                drives_size: false,
            },
            session,
            _proxy: proxy,
        })
    }

    pub async fn stop(self) -> Result<()> {
        self.session.close().await.context("closing portal session")?;
        Ok(())
    }
}
