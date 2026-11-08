//! Latency-test overlay and the matching measurement tool.
//!
//! With `latency_overlay = true`, every frame is stamped just before encoding
//! with the wall-clock time it was *captured* (milliseconds since the Unix
//! epoch). The stamp goes in twice:
//!
//! - as text, `HH:MM:SS.mmm UTC`, for photographing the TV next to a clock;
//! - as a block code in the top-left corner: 2 rows × 32 black/white blocks =
//!   48-bit timestamp + 16-bit check, big enough to survive compression.
//!
//! `castdeskd latency <url>` plays a stream, reads the block code from each
//! decoded frame and reports latency = now − capture time. Run on the laptop
//! itself it needs no clock sync, and covers capture → convert → encode →
//! MediaMTX → network stack → decode (not the receiver's display). Run on
//! another machine, both clocks must be NTP-synced: the error is the clock
//! offset (usually a few ms on a LAN).
//!
//! No `gstreamer-video` (its -dev headers aren't a build dependency here), so
//! planes are addressed with GStreamer's default layout. The stamping
//! `identity drop-allocation=true` makes the upstream converter allocate plain
//! system-memory buffers with that layout; anything else is detected by size
//! and left unstamped, with a warning.
//!
//! Testing: code layout, painting and reading are unit-tested, plus a real
//! encode → decode round trip (x264 + any H.264 decoder). The self-test turns
//! the overlay on and runs `castdeskd latency` against MediaMTX.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow, bail};
use gst::prelude::*;

pub const STAMP: &str = "latency_stamp";
pub const TEXT: &str = "latency_text";

const COLS: usize = 32;
const ROWS: usize = 2;
const BLACK: u8 = 16;
const WHITE: u8 = 235;

/// Where the block code sits, for a given frame width. One block of black
/// quiet zone surrounds the code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    pub block: usize,
}

impl Layout {
    pub fn new(width: u32) -> Self {
        // Even, so 4:2:0 chroma lines up with the blocks.
        Self { block: ((width as usize) / 80).max(4) & !1 }
    }
    pub fn width_px(&self) -> usize {
        (COLS + 2) * self.block
    }
    pub fn height_px(&self) -> usize {
        (ROWS + 2) * self.block
    }
    fn cell(&self, bit: usize) -> (usize, usize) {
        ((1 + bit % COLS) * self.block, (1 + bit / COLS) * self.block)
    }
    fn fits(&self, w: usize, h: usize) -> bool {
        self.width_px() <= w && self.height_px() <= h
    }
}

fn check(ms: u64) -> u64 {
    ((ms ^ (ms >> 16) ^ (ms >> 32)) & 0xffff) ^ 0xc0de
}

/// 48-bit milliseconds + 16-bit check.
pub fn encode(ms: u64) -> u64 {
    let ms = ms & 0xffff_ffff_ffff;
    (ms << 16) | check(ms)
}

pub fn decode(word: u64) -> Option<u64> {
    let ms = word >> 16;
    (check(ms) == word & 0xffff).then_some(ms)
}

fn ru2(n: usize) -> usize {
    (n + 1) & !1
}
fn ru4(n: usize) -> usize {
    (n + 3) & !3
}

/// GStreamer's default plane layout (no VideoMeta): (offset, stride) per
/// plane and the total size. Mirrors gst_video_info_set_format().
fn planes(format: &str, w: usize, h: usize) -> Option<(Vec<(usize, usize)>, usize)> {
    match format {
        "NV12" => {
            let s = ru4(w);
            let y = s * ru2(h);
            Some((vec![(0, s), (y, s)], y + s * ru2(h) / 2))
        }
        "I420" => {
            let s0 = ru4(w);
            let s1 = ru4(ru2(w) / 2);
            let o1 = s0 * ru2(h);
            let o2 = o1 + s1 * (ru2(h) / 2);
            Some((vec![(0, s0), (o1, s1), (o2, s1)], o2 + s1 * (ru2(h) / 2)))
        }
        "GRAY8" => {
            let s = ru4(w);
            Some((vec![(0, s)], s * h))
        }
        _ => None,
    }
}

/// Fill a `w`×`h` rectangle at (`x`, `y`) of the plane at `(offset, stride)`.
fn fill(data: &mut [u8], (offset, stride): (usize, usize), (x, y, w, h): (usize, usize, usize, usize), v: u8) {
    for row in y..y + h {
        let start = offset + row * stride + x;
        data[start..start + w].fill(v);
    }
}

/// Paint the code for `word` into an NV12 or I420 frame. False if the buffer
/// isn't in the default layout or the frame is too small.
pub fn paint(data: &mut [u8], format: &str, w: usize, h: usize, l: Layout, word: u64) -> bool {
    let Some((p, size)) = planes(format, w, h) else { return false };
    if data.len() != size || !l.fits(w, h) {
        return false;
    }
    let (cw, ch) = (l.width_px(), l.height_px());
    fill(data, p[0], (0, 0, cw, ch), BLACK);
    for bit in 0..ROWS * COLS {
        if (word >> (63 - bit)) & 1 == 1 {
            let (x, y) = l.cell(bit);
            fill(data, p[0], (x, y, l.block, l.block), WHITE);
        }
    }
    // Neutral chroma, so the code is pure black/white whatever was behind it.
    match format {
        // Interleaved UV: one byte pair per 2 pixels = cw bytes per row.
        "NV12" => fill(data, p[1], (0, 0, cw, ch / 2), 128),
        _ => {
            for &plane in &p[1..] {
                fill(data, plane, (0, 0, cw / 2, ch / 2), 128);
            }
        }
    }
    true
}

/// Read the code from a GRAY8 frame (or any frame's luma plane).
pub fn read(luma: &[u8], stride: usize, w: usize, h: usize, l: Layout) -> Option<u64> {
    if !l.fits(w, h) {
        return None;
    }
    // Average the middle half of each block: edges blur under compression.
    let (lo, hi) = (l.block / 4, l.block - l.block / 4);
    let mut word = 0u64;
    for bit in 0..ROWS * COLS {
        let (x, y) = l.cell(bit);
        let (mut sum, mut n) = (0u32, 0u32);
        for row in y + lo..y + hi {
            let start = row * stride + x;
            for &v in luma.get(start + lo..start + hi)? {
                sum += v as u32;
                n += 1;
            }
        }
        if sum / n.max(1) > 126 {
            word |= 1 << (63 - bit);
        }
    }
    decode(word)
}

/// gst-launch fragment, inserted after the final raw caps (format and size
/// fixed) and before the encoder queue.
pub fn description(width: u32, height: u32) -> String {
    let l = Layout::new(width);
    let pt = (height / 40).max(8);
    format!(
        "identity name={STAMP} drop-allocation=true \
         ! textoverlay name={TEXT} halignment=left valignment=top xpad={x} ypad={y} \
           font-desc=\"Monospace Bold {pt}\" shaded-background=true",
        x = l.block,
        y = l.height_px() + l.block,
    )
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64
}

fn hms(ms: u64) -> String {
    let s = ms / 1000;
    format!("{:02}:{:02}:{:02}.{:03}", s / 3600 % 24, s / 60 % 60, s % 60, ms % 1000)
}

fn caps_info(pad: &gst::Pad) -> Option<(String, usize, usize)> {
    let caps = pad.current_caps()?;
    let s = caps.structure(0)?;
    Some((
        s.get::<&str>("format").ok()?.to_string(),
        s.get::<i32>("width").ok()? as usize,
        s.get::<i32>("height").ok()? as usize,
    ))
}

/// Wire up the stamping probe in a pipeline built from `description`.
pub fn install(pipeline: &gst::Pipeline) -> Result<()> {
    let stamp = pipeline.by_name(STAMP).ok_or_else(|| anyhow!("no {STAMP}"))?;
    let text = pipeline.by_name(TEXT).ok_or_else(|| anyhow!("no {TEXT}"))?.downgrade();
    let element = stamp.downgrade();
    let warned = AtomicBool::new(false);
    let src = stamp.static_pad("src").ok_or_else(|| anyhow!("{STAMP} has no src pad"))?;
    src.add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
        let Some((format, w, h)) = caps_info(pad) else { return gst::PadProbeReturn::Ok };
        let Some(buffer) = info.buffer_mut() else { return gst::PadProbeReturn::Ok };
        // Capture time = now − how long the frame has been in the pipeline.
        // Live sources timestamp in running time, so that age is now − PTS.
        let age = match (element.upgrade().and_then(|e| e.current_running_time()), buffer.pts()) {
            (Some(now), Some(pts)) if now >= pts => (now - pts).mseconds().min(5_000),
            _ => 0,
        };
        let ms = now_ms().saturating_sub(age);
        if let Some(t) = text.upgrade() {
            t.set_property("text", format!("CastDesk latency  {} UTC", hms(ms)));
        }
        let buffer = buffer.make_mut();
        let painted = buffer
            .map_writable()
            .map(|mut m| paint(m.as_mut_slice(), &format, w, h, Layout::new(w as u32), encode(ms)))
            .unwrap_or(false);
        if !painted && !warned.swap(true, Ordering::Relaxed) {
            tracing::warn!(
                format,
                w,
                h,
                "latency overlay: frame isn't in the default {format} layout; block code skipped (text still shown)"
            );
        }
        gst::PadProbeReturn::Ok
    });
    Ok(())
}

/// What `castdeskd latency` measured.
#[derive(Debug, Default)]
pub struct Report {
    pub frames: usize,
    /// Frames decoded before the first readable code: a receiver joining
    /// mid-stream decodes garbage until the first keyframe.
    pub before_first: usize,
    /// Latency of each frame whose code was readable, in ms (negative = the
    /// receiver's clock is behind the sender's).
    pub samples: Vec<i64>,
}

impl Report {
    fn pct(sorted: &[i64], p: usize) -> i64 {
        sorted[(sorted.len() - 1) * p / 100]
    }

    pub fn summary(&self) -> String {
        if self.samples.is_empty() {
            return format!(
                "{} frames decoded, none carried a readable latency code — is latency_overlay = true on that display?",
                self.frames
            );
        }
        let mut s = self.samples.clone();
        s.sort_unstable();
        let mut out = format!(
            "{} frames, {} with a code ({} before the first keyframe): latency min {} / median {} / p95 {} / max {} ms",
            self.frames,
            s.len(),
            self.before_first,
            s[0],
            Self::pct(&s, 50),
            Self::pct(&s, 95),
            s[s.len() - 1]
        );
        if s[0] < 0 {
            out += "\nnegative values: this machine's clock is behind the sender's; sync both with NTP (chrony/timesyncd)";
        }
        out
    }

    #[cfg(test)]
    pub fn median(&self) -> Option<i64> {
        let mut s = self.samples.clone();
        s.sort_unstable();
        (!s.is_empty()).then(|| Self::pct(&s, 50))
    }
}

/// Receiver side: decode `url` for `seconds` and read every frame's code.
/// Blocking. `tcp` picks the RTSP transport (ignored for HTTP/HLS URLs).
pub fn measure(url: &str, seconds: u64, tcp: bool) -> Result<Report> {
    if url.contains(['"', ' ', '!']) {
        bail!("unexpected characters in URL {url:?}");
    }
    let src = if url.starts_with("rtsp://") {
        let proto = if tcp { "tcp" } else { "udp" };
        format!("rtspsrc location=\"{url}\" protocols={proto} latency=0 ! decodebin")
    } else {
        format!("uridecodebin uri=\"{url}\"")
    };
    let desc = format!("{src} ! videoconvert ! video/x-raw,format=GRAY8 ! fakesink name=sink sync=false");
    measure_pipeline(&desc, Duration::from_secs(seconds), false)
}

/// Run any pipeline ending in `fakesink name=sink` (GRAY8 caps) and read
/// codes until EOS or `run_for` after the first frame. `stamp` also installs
/// the stamping probe, for pipelines that contain the overlay (tests).
fn measure_pipeline(desc: &str, run_for: Duration, stamp: bool) -> Result<Report> {
    let pipeline = gst::parse::launch(desc)
        .with_context(|| format!("building {desc}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;
    if stamp {
        install(&pipeline)?;
    }
    let report = Arc::new(Mutex::new(Report::default()));
    let sink = pipeline.by_name("sink").ok_or_else(|| anyhow!("no sink"))?;
    let r = report.clone();
    sink.static_pad("sink")
        .ok_or_else(|| anyhow!("sink has no pad"))?
        .add_probe(gst::PadProbeType::BUFFER, move |pad, info| {
            let now = now_ms() as i64;
            let (Some((_, w, h)), Some(buf)) = (caps_info(pad), info.buffer()) else {
                return gst::PadProbeReturn::Ok;
            };
            let stamp = buf.map_readable().ok().and_then(|m| {
                let (p, size) = planes("GRAY8", w, h)?;
                (m.len() == size).then(|| read(&m, p[0].1, w, h, Layout::new(w as u32)))?
            });
            let mut r = r.lock().unwrap();
            r.frames += 1;
            match stamp {
                Some(ms) => r.samples.push(now - ms as i64),
                None if r.samples.is_empty() => r.before_first += 1,
                None => {}
            }
            gst::PadProbeReturn::Ok
        });

    pipeline.set_state(gst::State::Playing).context("starting the receiver pipeline")?;
    let bus = pipeline.bus().ok_or_else(|| anyhow!("no bus"))?;
    // Connecting can take a while; the clock starts at the first frame.
    let connect_deadline = Instant::now() + Duration::from_secs(15);
    let mut first_frame: Option<Instant> = None;
    let result = loop {
        if let Some(msg) = bus.timed_pop_filtered(
            gst::ClockTime::from_mseconds(100),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            match msg.view() {
                gst::MessageView::Error(e) => break Err(anyhow!("receiver: {} ({:?})", e.error(), e.debug())),
                _ => break Ok(()),
            }
        }
        if first_frame.is_none() && report.lock().unwrap().frames > 0 {
            first_frame = Some(Instant::now());
        }
        match first_frame {
            Some(t) if t.elapsed() >= run_for => break Ok(()),
            None if Instant::now() > connect_deadline => break Err(anyhow!("no video within 15 s")),
            _ => {}
        }
    };
    let _ = pipeline.set_state(gst::State::Null);
    result?;
    let r = std::mem::take(&mut *report.lock().unwrap());
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn code_roundtrip_and_rejects_noise() {
        let ms = 1_793_000_000_123;
        assert_eq!(decode(encode(ms)), Some(ms));
        assert_eq!(decode(0), None, "black frame must not read as a time");
        assert_eq!(decode(u64::MAX), None, "white frame must not read as a time");
        assert_eq!(decode(encode(ms) ^ (1 << 40)), None, "one flipped bit is caught");
    }

    #[test]
    fn layout_fits_every_allowed_size() {
        for (w, h) in [(320, 240), (1280, 720), (1366, 768), (1920, 1080), (3840, 2160), (7680, 4320)] {
            let l = Layout::new(w);
            assert!(l.block >= 4 && l.block.is_multiple_of(2));
            assert!(l.fits(w as usize, h as usize), "{w}x{h}");
            // The code stays in the top-left corner, under half the width.
            assert!(l.width_px() * 2 <= w as usize, "{w}x{h}");
        }
    }

    #[test]
    fn paint_then_read_nv12_and_i420() {
        for format in ["NV12", "I420"] {
            let (w, h) = (1366, 768); // odd-ish size: stride padding matters
            let (p, size) = planes(format, w, h).unwrap();
            let mut frame = vec![90u8; size];
            let l = Layout::new(w as u32);
            let ms = 1_793_000_000_456;
            assert!(paint(&mut frame, format, w, h, l, encode(ms)));
            assert_eq!(read(&frame[..p[0].1 * h], p[0].1, w, h, l), Some(ms), "{format}");
            // Chroma under the code is neutral.
            assert_eq!(frame[p[1].0], 128);
            // Wrong-size buffers are refused, not scribbled over.
            assert!(!paint(&mut frame[..size - 1], format, w, h, l, encode(ms)));
        }
        assert!(!paint(&mut [0; 64], "BGRx", 4, 4, Layout::new(4), 0));
    }

    /// Real encode → decode: the code survives H.264 at a low bitrate.
    #[test]
    fn survives_h264_roundtrip() {
        gst::init().unwrap();
        let decoder = ["avdec_h264", "vah264dec", "openh264dec", "vaapih264dec"]
            .into_iter()
            .find(|d| gst::ElementFactory::find(d).is_some());
        let (true, Some(dec)) = (gst::ElementFactory::find("x264enc").is_some(), decoder) else {
            eprintln!("needs x264enc and an H.264 decoder; skipping");
            return;
        };
        let desc = format!(
            "videotestsrc is-live=true pattern=ball num-buffers=60 \
             ! video/x-raw,width=640,height=360,framerate=30/1 ! videoconvert \
             ! video/x-raw,format=NV12 ! {overlay} \
             ! x264enc tune=zerolatency bitrate=500 bframes=0 ! h264parse ! {dec} \
             ! videoconvert ! video/x-raw,format=GRAY8 ! fakesink name=sink sync=false",
            overlay = description(640, 360),
        );
        let r = measure_pipeline(&desc, Duration::from_secs(30), true).unwrap();
        eprintln!("{}", r.summary());
        assert!(r.frames >= 50, "{}", r.summary());
        // Every frame after the first keyframe must be readable.
        assert_eq!(r.samples.len() + r.before_first, r.frames, "{}", r.summary());
        let med = r.median().unwrap();
        assert!((0..2000).contains(&med), "{}", r.summary());
    }
}
