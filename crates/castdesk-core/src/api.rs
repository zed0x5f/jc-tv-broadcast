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
    /// STUB (stage b): DisplayLink evdi kernel module. Not implemented; the
    /// daemon rejects it. Left out of `ALL` so front ends don't offer it.
    Evdi,
}

impl BackendKind {
    /// Backends a front end should offer. Excludes unimplemented stubs.
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
            BackendKind::Evdi => "evdi",
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
        if !self.width.is_multiple_of(2) || !self.height.is_multiple_of(2) {
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
        if self.codec == Codec::H264 && self.h264_level().is_none() {
            return Err(format!(
                "{}x{} @ {} fps at {} kb/s is beyond H.264 level 5.2, which almost no receiver decodes \
                 (max ≈ 4096x2160 @ 60): lower the resolution, fps or bitrate, or use H.265",
                self.width, self.height, self.fps, self.bitrate_kbps
            ));
        }
        Ok(())
    }

    /// Lowest H.264 level (High profile) that can carry these settings, e.g.
    /// "4.2"; `None` if it needs more than 5.2. Limits from ITU-T H.264
    /// Table A-1: macroblocks per frame, macroblocks per second, bitrate
    /// (High profile allows 1.25× the base bitrate).
    pub fn h264_level(&self) -> Option<&'static str> {
        // (level, MaxFS, MaxMBPS, MaxBR kb/s for Baseline/Main)
        const LEVELS: &[(&str, u64, u64, u64)] = &[
            ("3.1", 3_600, 108_000, 14_000),
            ("3.2", 5_120, 216_000, 20_000),
            ("4", 8_192, 245_760, 20_000),
            ("4.1", 8_192, 245_760, 50_000),
            ("4.2", 8_704, 522_240, 50_000),
            ("5", 22_080, 589_824, 135_000),
            ("5.1", 36_864, 983_040, 240_000),
            ("5.2", 36_864, 2_073_600, 240_000),
        ];
        let mbs = |px: u32| u64::from(px.div_ceil(16));
        let (w, h) = (mbs(self.width), mbs(self.height));
        let frame = w * h;
        let rate = frame * u64::from(self.fps);
        let br = u64::from(self.bitrate_kbps);
        LEVELS
            .iter()
            .find(|&&(_, max_fs, max_mbps, max_br)| {
                // Neither side may exceed sqrt(8 * MaxFS) macroblocks.
                let side = ((8 * max_fs) as f64).sqrt() as u64;
                frame <= max_fs && w <= side && h <= side && rate <= max_mbps && br * 4 <= max_br * 5
            })
            .map(|l| l.0)
    }

    /// Settings that work but that some receivers can't play. Shown to the
    /// user; never an error.
    pub fn compat_warnings(&self) -> Vec<String> {
        let mut w = Vec::new();
        match self.codec {
            Codec::H265 => w.push(
                "H.265: browsers, many older TVs and some Android TV players can't decode it; use H.264 if a receiver shows nothing".into(),
            ),
            Codec::H264 => {
                if let Some(level) = self.h264_level()
                    && !matches!(level, "3.1" | "3.2" | "4" | "4.1" | "4.2")
                {
                    w.push(format!(
                        "needs H.264 level {level}: many TVs and streaming sticks only decode up to 4.2 (1080p60)"
                    ));
                }
            }
        }
        w
    }
}

/// Marker for features that are declared but not built yet. Validation
/// rejects them with this message so nothing silently does nothing.
pub fn not_implemented(feature: &str, stage: &str) -> String {
    format!("{feature} is not implemented yet (planned for stage {stage})")
}

/// STUB (stage b): an extra network output fed from the display's encoder
/// `tee`, next to the RTSP publish. Only the config shape exists; any entry
/// is rejected by [`DisplaySpec::validate`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Destination {
    /// SRT listener: receivers connect to srt://<host>:<port>.
    Srt {
        port: u16,
        #[serde(default = "default_srt_latency")]
        latency_ms: u32,
        #[serde(default)]
        passphrase: Option<String>,
    },
    /// MPEG-TS over UDP, to a unicast host or a multicast group.
    Udp {
        host: String,
        port: u16,
        /// Multicast TTL (ignored for unicast).
        #[serde(default = "default_ttl")]
        ttl: u32,
    },
}

fn default_srt_latency() -> u32 {
    120
}
fn default_ttl() -> u32 {
    1
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
    /// STUB (stage b): SRT / UDP outputs. Must be empty for now.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub destinations: Vec<Destination>,
    /// STUB (stage b): capture desktop audio (PipeWire sink monitor). Must be false.
    #[serde(default, skip_serializing_if = "is_false")]
    pub audio: bool,
    /// Burn the capture time into every frame (text + machine-readable block
    /// code, top-left) so `castdeskd latency <url>` can measure end-to-end
    /// latency. For testing: it covers part of the picture.
    #[serde(default, skip_serializing_if = "is_false")]
    pub latency_overlay: bool,
}

fn is_false(b: &bool) -> bool {
    !*b
}

impl DisplaySpec {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            backend: BackendKind::Auto,
            placement: Placement::RightOf,
            video: VideoSettings::default(),
            destinations: Vec::new(),
            audio: false,
            latency_overlay: false,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        validate_name(&self.name)?;
        self.video.validate()?;
        // Stage (b) stubs: reject rather than accept-and-ignore.
        if self.backend == BackendKind::Evdi {
            return Err(not_implemented("the evdi backend", "b"));
        }
        if !self.destinations.is_empty() {
            return Err(not_implemented("SRT/UDP destination support", "b"));
        }
        if self.audio {
            return Err(not_implemented("audio", "b"));
        }
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    fn v(width: u32, height: u32, fps: u32, bitrate_kbps: u32) -> VideoSettings {
        VideoSettings { width, height, fps, bitrate_kbps, ..Default::default() }
    }

    #[test]
    fn h264_levels() {
        assert_eq!(v(1280, 720, 30, 4_000).h264_level(), Some("3.1"));
        assert_eq!(v(1280, 720, 60, 8_000).h264_level(), Some("3.2"));
        assert_eq!(v(1920, 1080, 30, 12_000).h264_level(), Some("4"));
        assert_eq!(v(1920, 1080, 30, 30_000).h264_level(), Some("4.1"));
        // The default (1080p60, 12 Mb/s) fits the level every 1080p TV decodes.
        assert_eq!(VideoSettings::default().h264_level(), Some("4.2"));
        assert_eq!(v(2560, 1440, 60, 20_000).h264_level(), Some("5.1"));
        assert_eq!(v(3840, 2160, 30, 20_000).h264_level(), Some("5.1"));
        assert_eq!(v(3840, 2160, 60, 40_000).h264_level(), Some("5.2"));
        assert_eq!(v(7680, 4320, 30, 40_000).h264_level(), None);
        // 1366 wide is not a multiple of 16; it still fits 4.x.
        assert_eq!(v(1366, 768, 60, 12_000).h264_level(), Some("4.2"));
    }

    #[test]
    fn validation_rejects_impossible_h264_only() {
        assert!(VideoSettings::default().validate().is_ok());
        let eight_k = v(7680, 4320, 30, 40_000);
        assert!(eight_k.validate().unwrap_err().contains("level 5.2"));
        let hevc = VideoSettings { codec: Codec::H265, ..eight_k };
        assert!(hevc.validate().is_ok());
    }

    #[test]
    fn warnings() {
        assert!(VideoSettings::default().compat_warnings().is_empty());
        assert!(v(3840, 2160, 30, 20_000).compat_warnings()[0].contains("level 5.1"));
        let hevc = VideoSettings { codec: Codec::H265, ..Default::default() };
        assert!(hevc.compat_warnings()[0].contains("H.265"));
    }
}
