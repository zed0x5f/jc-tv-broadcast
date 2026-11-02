//! X11 fallback: grow the root framebuffer (`xrandr --fb`) and declare a RandR
//! *monitor* on the new, unscanned region (`xrandr --setmonitor … none`).
//! Window managers that honour RandR 1.5 monitors (GNOME/Xorg, KDE/X11, XFCE,
//! i3, …) treat it as a real screen; we capture that region with ximagesrc.
//!
//! Only right-of / below placement is supported: X coordinates can't be
//! negative, and shifting the physical outputs would move your laptop screen.

use anyhow::{Context, Result, anyhow, bail};
use castdesk_core::{DisplaySpec, Placement};
use tokio::process::Command;

use super::CaptureSource;

pub struct X11Display {
    pub monitor: String,
    pub source: CaptureSource,
    display: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Geom {
    pub x: i32,
    pub y: i32,
    pub w: u32,
    pub h: u32,
}

/// `Screen 0: minimum 8 x 8, current 1920 x 1080, maximum 32767 x 32767`
pub fn parse_screen_size(query: &str) -> Option<(u32, u32)> {
    let line = query.lines().find(|l| l.starts_with("Screen "))?;
    let cur = line.split("current ").nth(1)?;
    let mut it = cur.split(',').next()?.split(" x ");
    Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?))
}

/// `1920/344x1080/194+0+0` or `1920x1080+0+0`
pub fn parse_geom(s: &str) -> Option<Geom> {
    let (size, rest) = s.split_once('+')?;
    let (x, y) = rest.split_once('+')?;
    let (w, h) = size.split_once('x')?;
    let num = |p: &str| p.split('/').next()?.parse::<u32>().ok();
    Some(Geom {
        x: x.parse().ok()?,
        y: y.parse().ok()?,
        w: num(w)?,
        h: num(h)?,
    })
}

/// Primary (or first connected) output geometry from `xrandr --query`.
pub fn parse_primary(query: &str) -> Option<Geom> {
    let connected = query
        .lines()
        .filter(|l| l.contains(" connected") && !l.starts_with(' '));
    let pick = connected.clone().find(|l| l.contains(" primary ")).or_else(|| {
        connected.clone().find(|l| {
            l.split_whitespace()
                .nth(2)
                .is_some_and(|t| t.contains('x') && t.contains('+'))
        })
    })?;
    pick.split_whitespace().find_map(parse_geom)
}

/// `xrandr --listmonitors` → (name, geometry)
pub fn parse_monitors(list: &str) -> Vec<(String, Geom)> {
    list.lines()
        .skip(1)
        .filter_map(|l| {
            let mut parts = l.split_whitespace();
            parts.next()?; // "0:"
            let name = parts.next()?.trim_start_matches(['+', '*']).to_string();
            let geom = parse_geom(parts.next()?)?;
            Some((name, geom))
        })
        .collect()
}

/// Where the new region goes, given current framebuffer and primary output.
pub fn region(fb: (u32, u32), primary: Geom, w: u32, h: u32, placement: Placement) -> Result<(Geom, (u32, u32))> {
    let (x, y) = match placement {
        Placement::RightOf => (fb.0 as i32, primary.y),
        Placement::Below => (primary.x, fb.1 as i32),
        p => bail!("X11 backend supports right-of and below only (got {})", p.label()),
    };
    let new_fb = (
        fb.0.max(x as u32 + w),
        fb.1.max(y as u32 + h),
    );
    Ok((Geom { x, y, w, h }, new_fb))
}

fn mm(px: u32) -> u32 {
    (px as f64 * 25.4 / 96.0).round() as u32
}

async fn xrandr(display: &str, args: &[&str]) -> Result<String> {
    let out = Command::new("xrandr")
        .env("DISPLAY", display)
        .args(args)
        .output()
        .await
        .context("running xrandr (install x11-xserver-utils)")?;
    if !out.status.success() {
        bail!("xrandr {}: {}", args.join(" "), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

impl X11Display {
    pub async fn create(spec: &DisplaySpec) -> Result<Self> {
        let display = std::env::var("DISPLAY").map_err(|_| anyhow!("DISPLAY is not set"))?;
        let monitor = format!("CastDesk-{}", spec.name);
        let query = xrandr(&display, &["--query"]).await?;
        let fb = parse_screen_size(&query).ok_or_else(|| anyhow!("cannot parse xrandr screen size"))?;
        let primary = parse_primary(&query).ok_or_else(|| anyhow!("no connected output found"))?;
        let (g, new_fb) = region(fb, primary, spec.video.width, spec.video.height, spec.placement)?;

        // A stale monitor from a crashed run would make --setmonitor fail.
        xrandr(&display, &["--delmonitor", &monitor]).await.ok();
        xrandr(&display, &["--fb", &format!("{}x{}", new_fb.0, new_fb.1)]).await?;
        let spec_geom = format!("{}/{}x{}/{}+{}+{}", g.w, mm(g.w), g.h, mm(g.h), g.x, g.y);
        if let Err(e) = xrandr(&display, &["--setmonitor", &monitor, &spec_geom, "none"]).await {
            xrandr(&display, &["--fb", &format!("{}x{}", fb.0, fb.1)]).await.ok();
            return Err(e);
        }
        tracing::info!(monitor, geom = spec_geom, "X11 virtual monitor created");
        Ok(Self {
            source: CaptureSource::X11 {
                display: display.clone(),
                x: g.x,
                y: g.y,
                width: g.w,
                height: g.h,
            },
            monitor,
            display,
        })
    }

    pub async fn remove(self) -> Result<()> {
        xrandr(&self.display, &["--delmonitor", &self.monitor]).await?;
        // Shrink the framebuffer to what remaining monitors need.
        let list = xrandr(&self.display, &["--listmonitors"]).await?;
        let (w, h) = parse_monitors(&list).iter().fold((0, 0), |(w, h), (_, g)| {
            (w.max(g.x as u32 + g.w), h.max(g.y as u32 + g.h))
        });
        if w > 0 && h > 0 {
            xrandr(&self.display, &["--fb", &format!("{w}x{h}")]).await?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const QUERY: &str = "Screen 0: minimum 320 x 200, current 1920 x 1200, maximum 16384 x 16384
eDP-1 connected primary 1920x1200+0+0 (normal left inverted right x axis y axis) 302mm x 189mm
   1920x1200     60.00*+
HDMI-1 disconnected (normal left inverted right x axis y axis)
";

    #[test]
    fn parses_query() {
        assert_eq!(parse_screen_size(QUERY), Some((1920, 1200)));
        assert_eq!(parse_primary(QUERY), Some(Geom { x: 0, y: 0, w: 1920, h: 1200 }));
    }

    #[test]
    fn parses_monitor_list() {
        let list = "Monitors: 2\n 0: +*eDP-1 1920/302x1200/189+0+0  eDP-1\n 1: CastDesk-tv1 1920/508x1080/286+1920+0  none\n";
        let m = parse_monitors(list);
        assert_eq!(m.len(), 2);
        assert_eq!(m[1].0, "CastDesk-tv1");
        assert_eq!(m[1].1, Geom { x: 1920, y: 0, w: 1920, h: 1080 });
    }

    #[test]
    fn stacks_regions() {
        let p = Geom { x: 0, y: 0, w: 1920, h: 1200 };
        let (g, fb) = region((1920, 1200), p, 1920, 1080, Placement::RightOf).unwrap();
        assert_eq!((g.x, g.y, fb), (1920, 0, (3840, 1200)));
        // second display goes right of the first
        let (g2, fb2) = region(fb, p, 1280, 720, Placement::RightOf).unwrap();
        assert_eq!((g2.x, fb2), (3840, (5120, 1200)));
        let (g3, _) = region((1920, 1200), p, 1920, 1080, Placement::Below).unwrap();
        assert_eq!((g3.x, g3.y), (0, 1200));
        assert!(region((1920, 1200), p, 1, 1, Placement::LeftOf).is_err());
    }
}
