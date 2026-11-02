//! Well-known file locations (XDG).

use std::path::PathBuf;

/// `~/.config/castdesk/config.toml`
pub fn config_file() -> PathBuf {
    if let Ok(p) = std::env::var("CASTDESK_CONFIG") {
        return PathBuf::from(p);
    }
    dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("castdesk")
        .join("config.toml")
}

/// `$XDG_RUNTIME_DIR/castdesk` — token file, generated MediaMTX config.
pub fn runtime_dir() -> PathBuf {
    if let Ok(p) = std::env::var("CASTDESK_RUNTIME_DIR") {
        return PathBuf::from(p);
    }
    dirs::runtime_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("castdesk")
}

/// API bearer token written by the daemon (mode 0600).
pub fn token_file() -> PathBuf {
    runtime_dir().join("token")
}

/// `~/.local/state/castdesk` — portal restore tokens etc.
pub fn state_dir() -> PathBuf {
    dirs::state_dir()
        .or_else(dirs::data_local_dir)
        .unwrap_or_else(|| PathBuf::from("."))
        .join("castdesk")
}

/// `~/.local/share/castdesk` — managed MediaMTX binary lives in `bin/`.
pub fn data_dir() -> PathBuf {
    dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("castdesk")
}
