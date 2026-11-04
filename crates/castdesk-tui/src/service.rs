//! Local machine management: start/stop the daemon, read its logs, clipboard.
//!
//! Testing: only ANSI stripping is unit-tested. Starting/stopping the daemon
//! (systemctl or background process), reading logs and the clipboard tools
//! are untested.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

use base64::Engine;

const UNIT: &str = "castdeskd.service";

fn fallback_log() -> PathBuf {
    castdesk_core::paths::state_dir().join("castdeskd.log")
}

fn unit_installed() -> bool {
    Command::new("systemctl")
        .args(["--user", "cat", UNIT])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

/// castdeskd next to this binary (dev builds), else $PATH.
fn daemon_binary() -> PathBuf {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.parent().map(|d| d.join("castdeskd")))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from("castdeskd"))
}

pub fn start_daemon() -> Result<String, String> {
    if unit_installed() {
        let out = Command::new("systemctl")
            .args(["--user", "start", UNIT])
            .output()
            .map_err(|e| e.to_string())?;
        return if out.status.success() {
            Ok("started castdeskd.service".into())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        };
    }
    // No unit installed (dev checkout): run it detached, logging to a file.
    let log = fallback_log();
    if let Some(d) = log.parent() {
        let _ = std::fs::create_dir_all(d);
    }
    let file = std::fs::File::create(&log).map_err(|e| format!("{}: {e}", log.display()))?;
    let err = file.try_clone().map_err(|e| e.to_string())?;
    let bin = daemon_binary();
    let mut cmd = Command::new(&bin);
    cmd.arg("run").stdin(Stdio::null()).stdout(file).stderr(err);
    // New session so the daemon survives the TUI exiting.
    unsafe {
        use std::os::unix::process::CommandExt;
        cmd.pre_exec(|| {
            setsid();
            Ok(())
        });
    }
    cmd.spawn().map_err(|e| format!("spawning {}: {e}", bin.display()))?;
    Ok(format!("started {} (no systemd unit; log: {})", bin.display(), log.display()))
}

unsafe extern "C" {
    fn setsid() -> i32;
}

pub fn stop_daemon() -> Result<String, String> {
    if unit_installed() {
        let out = Command::new("systemctl")
            .args(["--user", "stop", UNIT])
            .output()
            .map_err(|e| e.to_string())?;
        return if out.status.success() {
            Ok("stopped castdeskd.service".into())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        };
    }
    let st = Command::new("pkill")
        .args(["-TERM", "-x", "castdeskd"])
        .status()
        .map_err(|e| e.to_string())?;
    if st.success() {
        Ok("sent SIGTERM to castdeskd".into())
    } else {
        Err("castdeskd is not running".into())
    }
}

pub fn read_logs(lines: usize) -> Vec<String> {
    if unit_installed()
        && let Ok(out) = Command::new("journalctl")
            .args(["--user", "-u", UNIT, "-n", &lines.to_string(), "--no-pager", "-o", "short-iso"])
            .output()
        {
            let text = String::from_utf8_lossy(&out.stdout);
            let v: Vec<String> = text.lines().map(strip_ansi).collect();
            if !v.is_empty() {
                return v;
            }
        }
    match std::fs::read_to_string(fallback_log()) {
        Ok(t) => {
            let all: Vec<String> = t.lines().map(strip_ansi).collect();
            all[all.len().saturating_sub(lines)..].to_vec()
        }
        Err(_) => vec!["no logs: castdeskd.service not installed and no fallback log yet".into()],
    }
}

pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\x1b' {
            if chars.peek() == Some(&'[') {
                chars.next();
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Copy to clipboard: wl-copy / xclip / xsel, plus OSC 52 for terminals
/// (incl. over SSH) that support it.
pub fn copy(text: &str) -> String {
    let osc = format!(
        "\x1b]52;c;{}\x07",
        base64::engine::general_purpose::STANDARD.encode(text)
    );
    let _ = std::io::stdout().write_all(osc.as_bytes());
    let _ = std::io::stdout().flush();

    let tools: &[(&str, &[&str])] = if std::env::var_os("WAYLAND_DISPLAY").is_some() {
        &[("wl-copy", &[]), ("xclip", &["-selection", "clipboard"]), ("xsel", &["-ib"])]
    } else {
        &[("xclip", &["-selection", "clipboard"]), ("xsel", &["-ib"])]
    };
    for (tool, args) in tools {
        if let Ok(mut child) = Command::new(tool)
            .args(*args)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
        {
            if let Some(mut stdin) = child.stdin.take() {
                let _ = stdin.write_all(text.as_bytes());
            }
            if child.wait().is_ok_and(|s| s.success()) {
                return format!("copied {text} ({tool})");
            }
        }
    }
    format!("copied {text} (OSC 52 — install wl-clipboard or xclip if it didn't land)")
}

#[cfg(test)]
mod tests {
    #[test]
    fn strips_ansi() {
        assert_eq!(super::strip_ansi("\x1b[2m2026\x1b[0m \x1b[32m INFO\x1b[0m x"), "2026  INFO x");
    }
}
