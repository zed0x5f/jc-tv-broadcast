//! Sway / Hyprland: create a headless output via the compositor's IPC, set its
//! mode and position, then capture it through the portal (MONITOR source).
//!
//! xdg-desktop-portal-wlr shows an output chooser on first use; pick the
//! `HEADLESS-n` output. To skip the chooser, see README "Sway".
//!
//! Testing: output-list parsing and placement math are unit-tested.
//! The swaymsg/hyprctl commands and the portal capture have never run against
//! a real Sway or Hyprland session.

use anyhow::{Context, Result, anyhow, bail};
use castdesk_core::{DisplaySpec, Placement, SessionInfo};
use serde::Deserialize;
use tokio::process::Command;

use super::portal::PortalDisplay;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Compositor {
    Sway,
    Hyprland,
}

pub struct WlrootsDisplay {
    pub output: String,
    pub portal: PortalDisplay,
    compositor: Compositor,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub width: i32,
    pub height: i32,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SwayOutput {
    pub name: String,
    #[serde(default)]
    pub active: bool,
    #[serde(default)]
    pub focused: bool,
    pub rect: Rect,
}

async fn run(cmd: &str, args: &[&str]) -> Result<String> {
    let out = Command::new(cmd)
        .args(args)
        .output()
        .await
        .with_context(|| format!("running {cmd}"))?;
    let stdout = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        bail!(
            "{cmd} {}: {}{}",
            args.join(" "),
            stdout.trim(),
            String::from_utf8_lossy(&out.stderr).trim()
        );
    }
    Ok(stdout)
}

async fn sway_outputs() -> Result<Vec<SwayOutput>> {
    let json = run("swaymsg", &["-t", "get_outputs", "-r"]).await?;
    Ok(serde_json::from_str(&json)?)
}

/// Pick the reference (laptop) output: focused non-headless, else first active.
pub fn reference_output(outputs: &[SwayOutput]) -> Option<&SwayOutput> {
    let real = |o: &&SwayOutput| o.active && !o.name.starts_with("HEADLESS-");
    outputs
        .iter()
        .filter(real)
        .find(|o| o.focused)
        .or_else(|| outputs.iter().find(real))
}

/// Logical position for a new output next to `reference`.
pub fn place(reference: &Rect, w: i32, h: i32, placement: Placement) -> (i32, i32) {
    match placement {
        Placement::RightOf => (reference.x + reference.width, reference.y),
        Placement::LeftOf => (reference.x - w, reference.y),
        Placement::Above => (reference.x, reference.y - h),
        Placement::Below => (reference.x, reference.y + reference.height),
    }
}

impl WlrootsDisplay {
    pub async fn create(spec: &DisplaySpec, session: &SessionInfo) -> Result<Self> {
        let compositor = match session.desktop.as_str() {
            "sway" => Compositor::Sway,
            "Hyprland" => Compositor::Hyprland,
            d => bail!("wlroots backend supports sway and Hyprland, not {d}"),
        };
        let v = &spec.video;
        let output = match compositor {
            Compositor::Sway => {
                let before = sway_outputs().await?;
                run("swaymsg", &["create_output"]).await?;
                let after = sway_outputs().await?;
                let new = after
                    .iter()
                    .find(|o| !before.iter().any(|b| b.name == o.name))
                    .ok_or_else(|| anyhow!("swaymsg create_output produced no new output"))?
                    .name
                    .clone();
                let (x, y) = reference_output(&before)
                    .map(|r| place(&r.rect, v.width as i32, v.height as i32, spec.placement))
                    .unwrap_or((0, 0));
                run("swaymsg", &[
                    "output",
                    &new,
                    "mode",
                    &format!("{}x{}@{}Hz", v.width, v.height, v.fps),
                    "pos",
                    &x.to_string(),
                    &y.to_string(),
                ])
                .await?;
                new
            }
            Compositor::Hyprland => {
                let name = format!("castdesk-{}", spec.name);
                run("hyprctl", &["output", "create", "headless", &name]).await?;
                let pos = match spec.placement {
                    Placement::RightOf => "auto-right",
                    Placement::LeftOf => "auto-left",
                    Placement::Above => "auto-up",
                    Placement::Below => "auto-down",
                };
                run("hyprctl", &[
                    "keyword",
                    "monitor",
                    &format!("{name},{}x{}@{},{pos},1", v.width, v.height, v.fps),
                ])
                .await?;
                name
            }
        };
        tracing::info!(display = %spec.name, output, "headless output created");

        match PortalDisplay::capture_monitor(spec, &output).await {
            Ok(portal) => Ok(Self {
                output,
                portal,
                compositor,
            }),
            Err(e) => {
                remove_output(compositor, &output).await.ok();
                Err(e)
            }
        }
    }

    pub async fn remove(self) -> Result<()> {
        self.portal.stop().await.ok();
        remove_output(self.compositor, &self.output).await
    }
}

async fn remove_output(c: Compositor, output: &str) -> Result<()> {
    match c {
        Compositor::Sway => run("swaymsg", &["output", output, "unplug"]).await.map(|_| ()),
        Compositor::Hyprland => run("hyprctl", &["output", "remove", output]).await.map(|_| ()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const OUTPUTS: &str = r#"[
      {"name":"eDP-1","active":true,"focused":true,"rect":{"x":0,"y":0,"width":1536,"height":960}},
      {"name":"HEADLESS-1","active":true,"focused":false,"rect":{"x":1536,"y":0,"width":1920,"height":1080}}
    ]"#;

    #[test]
    fn parses_and_finds_reference() {
        let outs: Vec<SwayOutput> = serde_json::from_str(OUTPUTS).unwrap();
        assert_eq!(reference_output(&outs).unwrap().name, "eDP-1");
    }

    #[test]
    fn placement_math() {
        let r = Rect { x: 0, y: 0, width: 1536, height: 960 };
        assert_eq!(place(&r, 1920, 1080, Placement::RightOf), (1536, 0));
        assert_eq!(place(&r, 1920, 1080, Placement::LeftOf), (-1920, 0));
        assert_eq!(place(&r, 1920, 1080, Placement::Above), (0, -1080));
        assert_eq!(place(&r, 1920, 1080, Placement::Below), (0, 960));
    }
}
