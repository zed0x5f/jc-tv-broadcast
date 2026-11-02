//! Detect the graphical session and decide which virtual-display backends to try.

use std::collections::HashMap;

use castdesk_core::{BackendKind, SessionInfo};

/// Snapshot the env vars that matter (separate fn so tests can inject a map).
pub fn current_env() -> HashMap<String, String> {
    [
        "XDG_SESSION_TYPE",
        "XDG_CURRENT_DESKTOP",
        "XDG_SESSION_DESKTOP",
        "WAYLAND_DISPLAY",
        "DISPLAY",
        "SWAYSOCK",
        "HYPRLAND_INSTANCE_SIGNATURE",
    ]
    .iter()
    .filter_map(|k| std::env::var(k).ok().map(|v| (k.to_string(), v)))
    .collect()
}

pub fn detect(env: &HashMap<String, String>) -> SessionInfo {
    let get = |k: &str| env.get(k).map(String::as_str).unwrap_or("");
    let desktop_raw = format!("{}:{}", get("XDG_CURRENT_DESKTOP"), get("XDG_SESSION_DESKTOP"));
    let desk = desktop_raw.to_ascii_lowercase();

    let kind = match get("XDG_SESSION_TYPE") {
        "wayland" => "wayland",
        "x11" => "x11",
        "tty" => "tty",
        _ if !get("WAYLAND_DISPLAY").is_empty() => "wayland",
        _ if !get("DISPLAY").is_empty() => "x11",
        _ => "unknown",
    }
    .to_string();

    let desktop = if !get("SWAYSOCK").is_empty() || desk.contains("sway") {
        "sway"
    } else if !get("HYPRLAND_INSTANCE_SIGNATURE").is_empty() || desk.contains("hyprland") {
        "Hyprland"
    } else if desk.contains("gnome") || desk.contains("ubuntu") || desk.contains("pop") {
        "GNOME"
    } else if desk.contains("kde") || desk.contains("plasma") {
        "KDE"
    } else if desk.trim_matches(':').is_empty() {
        "unknown"
    } else {
        get("XDG_CURRENT_DESKTOP")
    }
    .to_string();

    use BackendKind::*;
    let backend_order = match (kind.as_str(), desktop.as_str()) {
        ("wayland", "GNOME") => vec![Mutter, Portal],
        ("wayland", "KDE") => vec![Portal],
        ("wayland", "sway" | "Hyprland") => vec![Wlroots],
        ("wayland", _) => vec![Portal],
        ("x11", _) => vec![X11],
        _ => vec![],
    };

    SessionInfo {
        kind,
        desktop,
        backend_order,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use BackendKind::*;

    fn env(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect()
    }

    #[test]
    fn gnome_wayland_prefers_mutter() {
        let s = detect(&env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "ubuntu:GNOME"),
        ]));
        assert_eq!(s.desktop, "GNOME");
        assert_eq!(s.backend_order, vec![Mutter, Portal]);
    }

    #[test]
    fn kde_uses_portal() {
        let s = detect(&env(&[
            ("XDG_SESSION_TYPE", "wayland"),
            ("XDG_CURRENT_DESKTOP", "KDE"),
        ]));
        assert_eq!(s.backend_order, vec![Portal]);
    }

    #[test]
    fn sway_by_socket() {
        let s = detect(&env(&[
            ("WAYLAND_DISPLAY", "wayland-1"),
            ("SWAYSOCK", "/run/user/1000/sway-ipc.sock"),
        ]));
        assert_eq!(s.kind, "wayland");
        assert_eq!(s.desktop, "sway");
        assert_eq!(s.backend_order, vec![Wlroots]);
    }

    #[test]
    fn x11_any_desktop() {
        let s = detect(&env(&[
            ("XDG_SESSION_TYPE", "x11"),
            ("XDG_CURRENT_DESKTOP", "XFCE"),
            ("DISPLAY", ":0"),
        ]));
        assert_eq!(s.desktop, "XFCE");
        assert_eq!(s.backend_order, vec![X11]);
    }

    #[test]
    fn headless_has_no_backends() {
        let s = detect(&env(&[]));
        assert_eq!(s.kind, "unknown");
        assert!(s.backend_order.is_empty());
    }
}
