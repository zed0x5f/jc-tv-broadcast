//! Owns all displays. Each display runs as its own task ("actor") that:
//! creates the virtual monitor → builds the pipeline → watches the bus →
//! samples stats → restarts with backoff on failure.

use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use castdesk_core::{
    Config, DisplaySpec, DisplayState, DisplayStatus, EncoderInfo, SessionInfo, StreamStats,
    StreamUrl,
};
use futures_util::StreamExt;
use tokio::sync::{RwLock, mpsc};
use tokio::task::JoinHandle;

use crate::display::{self, DisplayHandle};
use crate::encoder;
use crate::mediamtx::MediaMtx;
use crate::pipeline::{self, PipelineParams, StreamPipeline};

/// Failures in a row before the virtual display itself is recreated.
const RECREATE_DISPLAY_AFTER: u32 = 3;
/// Pipeline running this long counts as healthy (resets the failure counter).
const HEALTHY_AFTER: Duration = Duration::from_secs(30);

pub struct Context {
    pub session: SessionInfo,
    pub encoders: Vec<EncoderInfo>,
    pub mediamtx: Arc<MediaMtx>,
    pub host: String,
}

enum Cmd {
    Stop,
    Restart,
}

struct Entry {
    status: Arc<Mutex<DisplayStatus>>,
    cmd: mpsc::Sender<Cmd>,
    task: JoinHandle<()>,
}

pub struct Manager {
    ctx: Arc<Context>,
    displays: RwLock<HashMap<String, Entry>>,
    config: Mutex<Config>,
    config_path: std::path::PathBuf,
}

impl Manager {
    pub fn new(ctx: Context, config: Config, config_path: std::path::PathBuf) -> Arc<Self> {
        Arc::new(Self {
            ctx: Arc::new(ctx),
            displays: RwLock::new(HashMap::new()),
            config: Mutex::new(config),
            config_path,
        })
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    pub async fn start_configured(&self) {
        let specs = self.config.lock().unwrap().displays.clone();
        for spec in specs {
            if let Err(e) = self.spawn(spec).await {
                tracing::error!("starting configured display: {e:#}");
            }
        }
    }

    pub async fn list(&self) -> Vec<DisplayStatus> {
        let map = self.displays.read().await;
        let mut v: Vec<_> = map.values().map(|e| e.status.lock().unwrap().clone()).collect();
        v.sort_by(|a, b| a.spec.name.cmp(&b.spec.name));
        v
    }

    pub async fn get(&self, name: &str) -> Option<DisplayStatus> {
        let map = self.displays.read().await;
        map.get(name).map(|e| e.status.lock().unwrap().clone())
    }

    /// Add a display, start it, and persist it to the config file.
    pub async fn add(&self, spec: DisplaySpec) -> Result<DisplayStatus> {
        spec.validate().map_err(|e| anyhow!(e))?;
        let status = self.spawn(spec.clone()).await?;
        self.persist(|cfg| {
            cfg.displays.retain(|d| d.name != spec.name);
            cfg.displays.push(spec);
        });
        Ok(status)
    }

    pub async fn remove(&self, name: &str) -> Result<()> {
        let entry = self.displays.write().await.remove(name);
        let Some(entry) = entry else {
            bail!("no display named {name:?}");
        };
        let _ = entry.cmd.send(Cmd::Stop).await;
        if tokio::time::timeout(Duration::from_secs(10), entry.task).await.is_err() {
            tracing::warn!(display = name, "actor did not stop in time");
        }
        self.persist(|cfg| cfg.displays.retain(|d| d.name != name));
        Ok(())
    }

    pub async fn restart(&self, name: &str) -> Result<()> {
        let map = self.displays.read().await;
        let e = map.get(name).ok_or_else(|| anyhow!("no display named {name:?}"))?;
        e.cmd.send(Cmd::Restart).await.map_err(|_| anyhow!("display actor gone"))?;
        Ok(())
    }

    pub async fn shutdown(&self) {
        let entries: Vec<_> = self.displays.write().await.drain().collect();
        for (_, e) in &entries {
            let _ = e.cmd.send(Cmd::Stop).await;
        }
        for (_, e) in entries {
            let _ = tokio::time::timeout(Duration::from_secs(10), e.task).await;
        }
    }

    fn persist(&self, f: impl FnOnce(&mut Config)) {
        let mut cfg = self.config.lock().unwrap();
        f(&mut cfg);
        if let Err(e) = cfg.save(&self.config_path) {
            tracing::error!("saving config: {e}");
        }
    }

    async fn spawn(&self, spec: DisplaySpec) -> Result<DisplayStatus> {
        let mut map = self.displays.write().await;
        if map.contains_key(&spec.name) {
            bail!("display {:?} already exists", spec.name);
        }
        // Fail fast on an impossible encoder choice instead of looping.
        let enc = encoder::choose(&self.ctx.encoders, &spec.video)?;
        let urls = vec![StreamUrl {
            kind: "rtsp".into(),
            url: format!("rtsp://{}:{}/{}", self.ctx.host, self.ctx.mediamtx.rtsp_port(), spec.name),
        }];
        let status = Arc::new(Mutex::new(DisplayStatus {
            spec: spec.clone(),
            state: DisplayState::Starting,
            backend: None,
            encoder: Some(enc.clone()),
            output_name: None,
            urls,
            stats: StreamStats::default(),
        }));
        let (tx, rx) = mpsc::channel(8);
        let actor = Actor {
            spec: spec.clone(),
            encoder: enc,
            ctx: self.ctx.clone(),
            status: status.clone(),
            rx,
        };
        let task = tokio::spawn(actor.run());
        let snapshot = status.lock().unwrap().clone();
        map.insert(spec.name.clone(), Entry { status, cmd: tx, task });
        Ok(snapshot)
    }
}

struct Actor {
    spec: DisplaySpec,
    encoder: String,
    ctx: Arc<Context>,
    status: Arc<Mutex<DisplayStatus>>,
    rx: mpsc::Receiver<Cmd>,
}

enum Outcome {
    Stop,
    Restart,
    /// Pipeline failed; `display_gone` = the capture source ended (EOS).
    Failed { error: String, display_gone: bool },
}

impl Actor {
    fn set_state(&self, s: DisplayState) {
        self.status.lock().unwrap().state = s;
    }

    async fn run(mut self) {
        let name = self.spec.name.clone();
        let mut display: Option<DisplayHandle> = None;
        let mut failures: u32 = 0;
        loop {
            if display.is_none() {
                self.set_state(DisplayState::Starting);
                match display::create(&self.spec, &self.ctx.session).await {
                    Ok((kind, handle)) => {
                        let mut st = self.status.lock().unwrap();
                        st.backend = Some(kind);
                        st.output_name = handle.output_name();
                        drop(st);
                        display = Some(handle);
                    }
                    Err(e) => {
                        // Don't retry automatically: it could pop portal dialogs repeatedly.
                        tracing::error!(display = %name, "creating virtual display: {e:#}");
                        self.set_state(DisplayState::Failed { error: format!("{e:#}") });
                        match self.rx.recv().await {
                            Some(Cmd::Restart) => continue,
                            Some(Cmd::Stop) | None => return,
                        }
                    }
                }
            }
            let handle = display.as_ref().unwrap();
            let started = Instant::now();
            let outcome = match self.start_pipeline(handle).await {
                Ok(sp) => self.watch(sp).await,
                Err(e) => Outcome::Failed { error: format!("{e:#}"), display_gone: false },
            };
            match outcome {
                Outcome::Stop => {
                    if let Some(d) = display.take() {
                        d.remove().await;
                    }
                    self.set_state(DisplayState::Stopped);
                    return;
                }
                Outcome::Restart => {
                    failures = 0;
                    if let Some(d) = display.take() {
                        d.remove().await;
                    }
                }
                Outcome::Failed { error, display_gone } => {
                    if started.elapsed() > HEALTHY_AFTER {
                        failures = 0;
                    }
                    failures += 1;
                    tracing::warn!(display = %name, failures, "pipeline failed: {error}");
                    if (display_gone || failures.is_multiple_of(RECREATE_DISPLAY_AFTER))
                        && let Some(d) = display.take() {
                            d.remove().await;
                        }
                    self.set_state(DisplayState::Restarting {
                        attempt: failures,
                        last_error: error,
                    });
                    let backoff = Duration::from_secs(1 << failures.min(5)).min(Duration::from_secs(30));
                    tokio::select! {
                        _ = tokio::time::sleep(backoff) => {}
                        cmd = self.rx.recv() => match cmd {
                            Some(Cmd::Restart) => { failures = 0; }
                            Some(Cmd::Stop) | None => {
                                if let Some(d) = display.take() { d.remove().await; }
                                self.set_state(DisplayState::Stopped);
                                return;
                            }
                        }
                    }
                }
            }
        }
    }

    async fn start_pipeline(&self, handle: &DisplayHandle) -> Result<StreamPipeline> {
        if !self.ctx.mediamtx.wait_ready(Duration::from_secs(10)).await {
            let msg = self.ctx.mediamtx.info().message.unwrap_or_default();
            bail!("RTSP server not listening on :{} {msg}", self.ctx.mediamtx.rtsp_port());
        }
        let params = PipelineParams {
            video: &self.spec.video,
            encoder: &self.encoder,
            source: handle.source(),
            rtsp_publish: format!(
                "rtsp://127.0.0.1:{}/{}",
                self.ctx.mediamtx.rtsp_port(),
                self.spec.name
            ),
        };
        let sp = pipeline::build(&params)?;
        sp.pipeline
            .set_state(gst::State::Playing)
            .map_err(|_| anyhow!("pipeline refused to start (see daemon log)"))?;
        Ok(sp)
    }

    async fn watch(&mut self, sp: StreamPipeline) -> Outcome {
        let bus = sp.pipeline.bus().expect("pipeline bus");
        let mut messages = bus.stream();
        // Fast first ticks so "running" shows up promptly, then 1 Hz.
        let mut tick = tokio::time::interval_at(
            tokio::time::Instant::now() + Duration::from_millis(250),
            Duration::from_secs(1),
        );
        let started = Instant::now();
        let (mut last_frames, mut last_bytes, mut last_t) = (0u64, 0u64, Instant::now());
        loop {
            tokio::select! {
                msg = messages.next() => {
                    let Some(msg) = msg else {
                        return Outcome::Failed { error: "bus closed".into(), display_gone: false };
                    };
                    use gst::MessageView;
                    match msg.view() {
                        MessageView::Error(e) => {
                            let src = e.src().map(|s| s.path_string().to_string()).unwrap_or_default();
                            let gone = src.contains("pipewiresrc");
                            return Outcome::Failed {
                                error: format!("{src}: {} ({})", e.error(), e.debug().unwrap_or_default()),
                                display_gone: gone,
                            };
                        }
                        MessageView::Eos(_) => {
                            return Outcome::Failed { error: "capture ended (EOS)".into(), display_gone: true };
                        }
                        MessageView::Warning(w) => {
                            tracing::warn!(display = %self.spec.name, "gst warning: {}", w.error());
                        }
                        _ => {}
                    }
                }
                _ = tick.tick() => {
                    let frames = sp.counters.frames.load(Ordering::Relaxed);
                    let bytes = sp.counters.bytes.load(Ordering::Relaxed);
                    let dt = last_t.elapsed().as_secs_f64().max(0.001);
                    let path = self.ctx.mediamtx.path(&self.spec.name).await;
                    let mut st = self.status.lock().unwrap();
                    // "running" = encoded frames flowing AND receivers can actually
                    // connect (MediaMTX has the path online). If the MediaMTX API
                    // is unreachable, frames flowing is the best we can know.
                    let live = frames > 0 && path.is_none_or(|p| p.online);
                    if live && st.state != DisplayState::Running {
                        st.state = DisplayState::Running;
                    }
                    st.stats = StreamStats {
                        bitrate_kbps: (bytes - last_bytes) as f64 * 8.0 / 1000.0 / dt,
                        fps: (frames - last_frames) as f64 / dt,
                        frames_total: frames,
                        dropped_total: sp.counters.dropped.load(Ordering::Relaxed),
                        rtsp_clients: path.map(|p| p.readers).unwrap_or(st.stats.rtsp_clients),
                        uptime_s: started.elapsed().as_secs(),
                    };
                    (last_frames, last_bytes, last_t) = (frames, bytes, Instant::now());
                }
                cmd = self.rx.recv() => {
                    return match cmd {
                        Some(Cmd::Restart) => Outcome::Restart,
                        Some(Cmd::Stop) | None => Outcome::Stop,
                    };
                }
            }
        }
    }
}

use gst::prelude::*;
