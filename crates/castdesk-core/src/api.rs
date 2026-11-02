//! Data model shared by the daemon's REST API and its clients.

use serde::{Deserialize, Serialize};

pub const API_PREFIX: &str = "/api/v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    #[default]
    H264,
    H265,
}

impl Codec {
    pub fn label(self) -> &'static str {
        match self {
            Codec::H264 => "H.264",
            Codec::H265 => "H.265",
        }
    }
}

/// Which hardware/software encoder family to use.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum EncoderPref {
    /// First working of VA-API → NVENC → software.
    #[default]
    Auto,
    Vaapi,
    Nvenc,
    Software,
}

/// How the virtual display is created.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum BackendKind {
    /// Detect from the running session.
    #[default]
    Auto,
    /// GNOME: org.gnome.Mutter.ScreenCast RecordVirtual (no dialog).
    Mutter,
    /// xdg-desktop-portal ScreenCast with source type VIRTUAL (KDE Plasma 6, GNOME fallback).
    Portal,
    /// Sway / Hyprland headless output + portal capture.
    Wlroots,
    /// X11: enlarge framebuffer + `xrandr --setmonitor`, capture with ximagesrc.
    X11,
    /// Synthetic test pattern, no display (headless verification).
    Test,
}

impl BackendKind {
    pub const ALL: [BackendKind; 6] = [
        BackendKind::Auto,
        BackendKind::Mutter,
        BackendKind::Portal,
        BackendKind::Wlroots,
        BackendKind::X11,
        BackendKind::Test,
    ];
    pub fn label(self) -> &'static str {
        match self {
            BackendKind::Auto => "auto",
            BackendKind::Mutter => "mutter",
            BackendKind::Portal => "portal",
            BackendKind::Wlroots => "wlroots",
            BackendKind::X11 => "x11",
            BackendKind::Test => "test",
        }
    }
}

/// Where the virtual display sits relative to the laptop's primary screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "kebab-case")]
pub enum Placement {
    #[default]
    RightOf,
    LeftOf,
    Above,
    Below,
}

impl Placement {
    pub const ALL: [Placement; 4] = [
        Placement::RightOf,
        Placement::LeftOf,
        Placement::Above,
        Placement::Below,
    ];
    pub fn label(self) -> &'static str {
        match self {
            Placement::RightOf => "right-of",
            Placement::LeftOf => "left-of",
            Placement::Above => "above",
            Placement::Below => "below",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct VideoSettings {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub codec: Codec,
    pub encoder: EncoderPref,
    /// Keyframe interval in seconds (GOP = fps * this).
    pub gop_seconds: f32,
}

impl Default for VideoSettings {
    fn default() -> Self {
        Self {
            width: 1920,
            height: 1080,
            fps: 60,
            bitrate_kbps: 12_000,
            codec: Codec::H264,
            encoder: EncoderPref::Auto,
            gop_seconds: 1.0,
        }
    }
}

impl VideoSettings {
    pub fn gop_frames(&self) -> u32 {
        ((self.fps as f32 * self.gop_seconds).round() as u32).max(1)
    }

    pub fn validate(&self) -> Result<(), String> {
        if !(320..=7680).contains(&self.width) || !(240..=4320).contains(&self.height) {
            return Err(format!("resolution {}x{} out of range", self.width, self.height));
        }
        if self.width % 2 != 0 || self.height % 2 != 0 {
            return Err("width and height must be even".into());
        }
        if !(1..=144).contains(&self.fps) {
            return Err(format!("fps {} out of range 1-144", self.fps));
        }
        if !(500..=100_000).contains(&self.bitrate_kbps) {
            return Err(format!("bitrate {} kb/s out of range 500-100000", self.bitrate_kbps));
        }
        if !(0.1..=10.0).contains(&self.gop_seconds) {
            return Err("gop_seconds must be 0.1-10".into());
        }
        Ok(())
    }
}

/// A virtual display and its stream, as configured by the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplaySpec {
    /// Also the RTSP path: rtsp://host:8554/<name>
    pub name: String,
    #[serde(default)]
    pub backend: BackendKind,
    #[serde(default)]
    pub placement: Placement,
    #[serde(flatten)]
    pub video: VideoSettings,
}

impl DisplaySpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backend: BackendKind::Auto,
            placement: Placement::RightOf,
            video: VideoSettings::default(),
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        self.video.validate()
    }
}

/// Display names become URL paths and output names: keep them boring.
pub fn validate_name(name: &str) -> Result<(), String> {
    let ok = !name.is_empty()
        && name.len() <= 32
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
        && !name.starts_with('-');
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid display name {name:?}: use 1-32 chars of a-z, 0-9, '-' or '_'"
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum DisplayState {
    Starting,
    Running,
    Restarting { attempt: u32, last_error: String },
    Failed { error: String },
    Stopped,
}

impl DisplayState {
    pub fn label(&self) -> &'static str {
        match self {
            DisplayState::Starting => "starting",
            DisplayState::Running => "running",
            DisplayState::Restarting { .. } => "restarting",
            DisplayState::Failed { .. } => "failed",
            DisplayState::Stopped => "stopped",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct StreamStats {
    /// Encoded bitrate over the last second.
    pub bitrate_kbps: f64,
    /// Encoded frames per second over the last second.
    pub fps: f64,
    pub frames_total: u64,
    /// Frames dropped by leaky queues / QoS.
    pub dropped_total: u64,
    /// RTSP readers reported by MediaMTX.
    pub rtsp_clients: u32,
    pub uptime_s: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamUrl {
    pub kind: String,
    pub url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DisplayStatus {
    pub spec: DisplaySpec,
    #[serde(flatten)]
    pub state: DisplayState,
    /// Backend actually in use (after `auto` resolution).
    pub backend: Option<BackendKind>,
    /// GStreamer encoder element in use, e.g. `vah264enc`.
    pub encoder: Option<String>,
    /// Compositor output / monitor name, e.g. `HEADLESS-1`.
    pub output_name: Option<String>,
    pub urls: Vec<StreamUrl>,
    pub stats: StreamStats,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionInfo {
    /// wayland / x11 / tty / unknown
    pub kind: String,
    /// e.g. GNOME, KDE, sway, Hyprland
    pub desktop: String,
    /// Backends worth trying, in order, for `auto`.
    pub backend_order: Vec<BackendKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderInfo {
    pub element: String,
    pub codec: Codec,
    pub family: EncoderPref,
    pub working: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MediaMtxInfo {
    pub managed: bool,
    pub running: bool,
    pub binary: Option<String>,
    pub rtsp_port: u16,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemInfo {
    pub version: String,
    pub host: String,
    pub session: SessionInfo,
    pub encoders: Vec<EncoderInfo>,
    pub mediamtx: MediaMtxInfo,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ApiError {
    pub error: String,
}
