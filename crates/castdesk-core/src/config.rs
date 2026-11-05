//! `~/.config/castdesk/config.toml`
//!
//! Testing: defaults, TOML round trip, minimal user file, validation, save/load
//! and stage (b) stub rejection are unit-tested.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::{DisplaySpec, VideoSettings};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default)]
pub struct Config {
    pub daemon: DaemonConfig,
    pub mediamtx: MediaMtxConfig,
    /// Defaults applied to new displays created without explicit settings.
    pub defaults: VideoSettings,
    /// Displays recreated every time the daemon starts.
    #[serde(rename = "display")]
    pub displays: Vec<DisplaySpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct DaemonConfig {
    /// Control API listen address. Keep it on loopback.
    pub listen: String,
    /// Address advertised in stream URLs. Empty = auto-detect LAN IP.
    pub advertise_host: String,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            listen: "127.0.0.1:7650".into(),
            advertise_host: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct MediaMtxConfig {
    /// Spawn and supervise MediaMTX. If false, an external server must be listening.
    pub managed: bool,
    /// Explicit binary path; otherwise ~/.local/share/castdesk/bin/mediamtx, then $PATH.
    pub binary: Option<PathBuf>,
    pub rtsp_port: u16,
    pub api_port: u16,
    /// RTP/RTCP UDP ports for UDP RTSP readers (open these in the firewall).
    pub rtp_port: u16,
    pub rtcp_port: u16,
    /// Also serve every display as HLS, for receivers without RTSP (browsers,
    /// smart-TV players, Kodi): http://host:<hls_port>/<name>/index.m3u8.
    /// MediaMTX repackages the same encode, so this costs no encoding.
    pub hls: bool,
    pub hls_port: u16,
    /// "mpegts" (plays almost anywhere, ~3-4 s latency), "fmp4", or
    /// "lowLatency" (LL-HLS, ~1 s, but older players and TVs may not support it).
    pub hls_variant: String,
    /// Also serve every display over WebRTC, for browsers:
    /// http://host:<webrtc_port>/<name>/ is a player page, `/whep` the endpoint.
    pub webrtc: bool,
    pub webrtc_port: u16,
    /// UDP port for WebRTC media (open it in the firewall).
    pub webrtc_udp_port: u16,
}

impl Default for MediaMtxConfig {
    fn default() -> Self {
        Self {
            managed: true,
            binary: None,
            rtsp_port: 8554,
            api_port: 9997,
            rtp_port: 8000,
            rtcp_port: 8001,
            hls: false,
            hls_port: 8888,
            hls_variant: "mpegts".into(),
            webrtc: false,
            webrtc_port: 8889,
            webrtc_udp_port: 8189,
        }
    }
}

pub const HLS_VARIANTS: [&str; 3] = ["mpegts", "fmp4", "lowLatency"];

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("reading {path}: {source}")]
    Io {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("parsing {path}: {source}")]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("serializing config: {0}")]
    Serialize(#[from] toml::ser::Error),
    #[error("{0}")]
    Invalid(String),
}

impl Config {
    /// Load the config, returning defaults if the file does not exist.
    pub fn load(path: &Path) -> Result<Self, ConfigError> {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.into(),
                    source,
                });
            }
        };
        let cfg: Config = toml::from_str(&text).map_err(|source| ConfigError::Parse {
            path: path.into(),
            source,
        })?;
        cfg.validate()?;
        Ok(cfg)
    }

    pub fn validate(&self) -> Result<(), ConfigError> {
        let mut seen = std::collections::HashSet::new();
        for d in &self.displays {
            d.validate().map_err(ConfigError::Invalid)?;
            if !seen.insert(&d.name) {
                return Err(ConfigError::Invalid(format!("duplicate display {:?}", d.name)));
            }
        }
        if !HLS_VARIANTS.contains(&self.mediamtx.hls_variant.as_str()) {
            return Err(ConfigError::Invalid(format!(
                "[mediamtx] hls_variant = {:?}: use one of {}",
                self.mediamtx.hls_variant,
                HLS_VARIANTS.join(", ")
            )));
        }
        self.defaults.validate().map_err(ConfigError::Invalid)
    }

    /// Atomically write the config (write temp file, then rename).
    pub fn save(&self, path: &Path) -> Result<(), ConfigError> {
        let text = toml::to_string_pretty(self)?;
        let io = |source| ConfigError::Io {
            path: path.into(),
            source,
        };
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(io)?;
        }
        let tmp = path.with_extension("toml.tmp");
        std::fs::write(&tmp, text).map_err(io)?;
        std::fs::rename(&tmp, path).map_err(io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::*;

    #[test]
    fn defaults_are_1080p60_low_latency() {
        let v = VideoSettings::default();
        assert_eq!((v.width, v.height, v.fps), (1920, 1080, 60));
        assert!((8_000..=15_000).contains(&v.bitrate_kbps));
        assert_eq!(v.gop_frames(), 60);
        assert_eq!(v.codec, Codec::H264);
    }

    #[test]
    fn roundtrip_toml() {
        let mut cfg = Config::default();
        let mut d = DisplaySpec::new("tv1");
        d.video.codec = Codec::H265;
        d.placement = Placement::LeftOf;
        cfg.displays.push(d);
        let text = toml::to_string_pretty(&cfg).unwrap();
        let back: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg, back);
    }

    #[test]
    fn parses_minimal_user_file() {
        let cfg: Config = toml::from_str(
            r#"
            [[display]]
            name = "living-room"
            width = 1280
            height = 720
            fps = 30
            "#,
        )
        .unwrap();
        cfg.validate().unwrap();
        let d = &cfg.displays[0];
        assert_eq!(d.video.width, 1280);
        assert_eq!(d.video.bitrate_kbps, 12_000);
        assert_eq!(d.backend, BackendKind::Auto);
    }

    #[test]
    fn rejects_bad_names_and_duplicates() {
        assert!(validate_name("TV 1").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_name("-x").is_err());
        assert!(validate_name("tv_1-a").is_ok());
        let mut cfg = Config::default();
        cfg.displays.push(DisplaySpec::new("a"));
        cfg.displays.push(DisplaySpec::new("a"));
        assert!(cfg.validate().is_err());
    }

    /// Stage (b) options parse (so the config shape is settled) but are
    /// rejected until implemented, instead of being silently ignored.
    #[test]
    fn stage_b_stubs_parse_but_are_rejected() {
        let cfg: Config = toml::from_str(
            r#"
            [[display]]
            name = "tv1"
            audio = true
            [[display.destinations]]
            kind = "srt"
            port = 9000
            [[display.destinations]]
            kind = "udp"
            host = "239.0.0.1"
            port = 5004
            "#,
        )
        .unwrap();
        let d = &cfg.displays[0];
        assert_eq!(d.destinations.len(), 2);
        assert!(matches!(d.destinations[0], Destination::Srt { latency_ms: 120, .. }));
        let err = cfg.validate().unwrap_err().to_string();
        assert!(err.contains("not implemented"), "{err}");

        for spec in [
            DisplaySpec { audio: true, ..DisplaySpec::new("a") },
            DisplaySpec { latency_overlay: true, ..DisplaySpec::new("a") },
            DisplaySpec { backend: BackendKind::Evdi, ..DisplaySpec::new("a") },
        ] {
            assert!(spec.validate().unwrap_err().contains("not implemented"));
        }
        // Defaults stay out of the saved file.
        let text = toml::to_string(&DisplaySpec::new("a")).unwrap();
        assert!(!text.contains("audio") && !text.contains("destinations"));
        assert!(!BackendKind::ALL.contains(&BackendKind::Evdi));
    }

    #[test]
    fn save_and_load() {
        let dir = std::env::temp_dir().join(format!("castdesk-test-{}", std::process::id()));
        let path = dir.join("config.toml");
        let mut cfg = Config::default();
        cfg.displays.push(DisplaySpec::new("x"));
        cfg.save(&path).unwrap();
        assert_eq!(Config::load(&path).unwrap(), cfg);
        std::fs::remove_dir_all(dir).ok();
    }
}
