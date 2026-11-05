//! One GStreamer pipeline per virtual display: capture → convert/scale →
//! encode once → parse → tee → destinations.
//!
//! Stage (a) has a single destination (RTSP publish to MediaMTX). The tee is
//! already in place so stage (b) adds SRT/UDP branches without touching the
//! encoder. Those branches, audio and the latency overlay are stubs for now:
//! see `destinations.rs`, `audio.rs` and `overlay.rs`.
//!
//! Testing: gst-launch strings are unit-tested for every source and encoder
//! variant, and a full pipeline is built with x264enc. The only source run
//! end-to-end (self-test) is `Test` (videotestsrc) + rtspclientsink, with
//! vah264enc and x264enc; it checks the receiver-facing profile, level, pixel
//! format and colour tags with ffprobe. `guard_encoded_queue` is unit-tested
//! with a real x264 encode into a queue drained too slowly: without the guard
//! undecodable frames come out, with it none do and keyframes are re-requested.
//! The PipeWire source has been run by hand with a Mutter virtual monitor +
//! vah264enc (that run found the `target-object` bug, see below). The X11
//! source is string-tested only.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{Context, Result, anyhow};
use castdesk_core::VideoSettings;
use gst::prelude::*;

use crate::display::CaptureSource;
use crate::encoder;

pub struct PipelineParams<'a> {
    pub video: &'a VideoSettings,
    pub encoder: &'a str,
    pub source: &'a CaptureSource,
    /// Where to publish, e.g. rtsp://127.0.0.1:8554/tv1
    pub rtsp_publish: String,
}

#[derive(Default)]
pub struct Counters {
    pub frames: AtomicU64,
    pub bytes: AtomicU64,
    pub dropped: AtomicU64,
}

pub struct StreamPipeline {
    pub pipeline: gst::Pipeline,
    pub counters: Arc<Counters>,
}

impl Drop for StreamPipeline {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// The capture half of the pipeline, as gst-launch syntax.
pub fn source_description(src: &CaptureSource, v: &VideoSettings) -> String {
    match src {
        CaptureSource::PipeWire {
            node_id,
            drives_size,
            ..
        } => {
            let fd = src.raw_fd().map(|fd| format!(" fd={fd}")).unwrap_or_default();
            // `path` takes the numeric node id that Mutter and the portal hand
            // us. `target-object` looks like its replacement but only matches
            // a node *name or serial*, so it fails with "target not found"
            // (verified on GNOME 50 / PipeWire 1.6). `path` is deprecated but
            // still supported.
            let target = format!("path={node_id}");
            // keepalive-time: screencasts are damage-driven; resend the last
            // frame on a static desktop so receivers never stall.
            let mut s = format!(
                "pipewiresrc{fd} {target} do-timestamp=true keepalive-time=500 always-copy=false"
            );
            if *drives_size {
                // Mutter sizes the virtual monitor from what we ask for.
                s += &format!(
                    " ! video/x-raw,width={},height={},max-framerate={}/1",
                    v.width, v.height, v.fps
                );
            }
            s
        }
        CaptureSource::X11 {
            display,
            x,
            y,
            width,
            height,
        } => format!(
            "ximagesrc display-name={display} startx={x} starty={y} endx={} endy={} use-damage=false show-pointer=true ! video/x-raw,framerate={}/1",
            *x as i64 + *width as i64 - 1,
            *y as i64 + *height as i64 - 1,
            v.fps
        ),
        CaptureSource::Test => format!(
            "videotestsrc is-live=true pattern=smpte ! video/x-raw,width={},height={},framerate={}/1 ! timeoverlay halignment=center valignment=center font-desc=\"Sans 48\"",
            v.width, v.height, v.fps
        ),
    }
}

/// Full pipeline description (encoder properties are applied afterwards).
///
/// Receiver compatibility is pinned here rather than left to each encoder:
/// - `colorimetry=bt709`: the stream is tagged BT.709 limited range, so TVs
///   don't guess the colour matrix or range (washed-out or crushed blacks).
/// - `encoder::output_caps`: a profile TV decoders accept, whichever encoder
///   family is picked.
///
/// The frame rate is deliberately left variable: a static screen sends a few
/// frames a second instead of encoding identical frames at full rate (see
/// `force_keyframes_by_time` for how joining stays fast).
pub fn description(p: &PipelineParams<'_>) -> String {
    let v = p.video;
    let parser = encoder::parser_for(v.codec);
    format!(
        "{src} \
         ! queue name=capq max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream \
         ! videoconvert n-threads=4 ! videoscale \
         ! video/x-raw,format={fmt},width={w},height={h},pixel-aspect-ratio=1/1,colorimetry=bt709 \
         ! queue name=encq max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream \
         ! {enc} name=enc \
         ! {enc_caps} \
         ! {parser} config-interval=-1 \
         ! identity name=stats silent=true \
         ! tee name=t allow-not-linked=true \
         t. ! queue name=q_rtsp max-size-buffers=0 max-size-bytes=0 max-size-time=500000000 leaky=downstream \
            ! rtspclientsink name=rtsp location={url} protocols=tcp latency=0",
        src = source_description(p.source, v),
        w = v.width,
        h = v.height,
        enc = p.encoder,
        enc_caps = encoder::output_caps(v.codec),
        fmt = encoder::input_format(p.encoder),
        url = p.rtsp_publish,
    )
}

/// Ask the encoder for a keyframe every `gop_seconds` of wall-clock time.
///
/// The encoder's own keyframe interval counts *frames* (fps × gop_seconds),
/// but screencasts only deliver frames when the screen changes: about 2 fps on
/// a static desktop, from `keepalive-time`. So a 30-frame GOP became a keyframe
/// every ~15 s, and a viewer joining a static screen waited 7-30 s for its
/// first picture (measured on GNOME). A timed GstForceKeyUnit request fixes
/// that whatever the frame rate. The thread ends when the pipeline is dropped.
fn force_keyframes_by_time(enc: &gst::Element, gop_seconds: f32) {
    let Some(pad) = enc.static_pad("sink") else { return };
    let weak = pad.downgrade();
    let every = std::time::Duration::from_secs_f32(gop_seconds);
    std::thread::Builder::new()
        .name("castdesk-keyframes".into())
        .spawn(move || loop {
            std::thread::sleep(every);
            let Some(pad) = weak.upgrade() else { break };
            let s = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            pad.send_event(gst::event::CustomDownstream::new(s));
        })
        .ok();
}

/// Make a leaky queue of *encoded* video safe to drop from.
///
/// Output queues after the tee are leaky so that one stalled destination can't
/// block the encoder (and every other destination). But dropping an encoded
/// frame breaks every frame after it until the next keyframe: receivers show
/// smeared or grey blocks for up to a GOP. So once the queue has dropped
/// anything, this holds back delta frames until a keyframe passes, and asks
/// the encoder for one right away so the gap is short.
///
/// `overrun` is emitted just before the queue drops, so the flag can be set a
/// frame early. That only costs one extra (unusable anyway) frame.
pub fn guard_encoded_queue(queue: &gst::Element, counters: Arc<Counters>) {
    use std::sync::atomic::AtomicBool;
    let broken = Arc::new(AtomicBool::new(false));

    let b = broken.clone();
    queue.connect("overrun", false, move |_| {
        counters.dropped.fetch_add(1, Ordering::Relaxed);
        b.store(true, Ordering::Release);
        None
    });

    let (Some(src), Some(sink)) = (queue.static_pad("src"), queue.static_pad("sink")) else {
        return;
    };
    let sink = sink.downgrade();
    // Runs on the queue's own streaming thread, which holds no encoder locks,
    // so sending the keyframe request upstream from here can't deadlock.
    let requested = AtomicBool::new(false);
    src.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
        if !broken.load(Ordering::Acquire) {
            return gst::PadProbeReturn::Ok;
        }
        let Some(buf) = info.buffer() else {
            return gst::PadProbeReturn::Ok;
        };
        if !buf.flags().contains(gst::BufferFlags::DELTA_UNIT) {
            broken.store(false, Ordering::Release);
            requested.store(false, Ordering::Relaxed);
            return gst::PadProbeReturn::Ok;
        }
        if !requested.swap(true, Ordering::Relaxed)
            && let Some(sink) = sink.upgrade()
        {
            let s = gst::Structure::builder("GstForceKeyUnit")
                .field("all-headers", true)
                .build();
            sink.push_event(gst::event::CustomUpstream::new(s));
        }
        gst::PadProbeReturn::Drop
    });
}

pub fn build(p: &PipelineParams<'_>) -> Result<StreamPipeline> {
    let desc = description(p);
    tracing::debug!(pipeline = %desc, "building");
    let pipeline = gst::parse::launch(&desc)
        .with_context(|| format!("parsing pipeline: {desc}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;

    let enc = pipeline.by_name("enc").ok_or_else(|| anyhow!("no encoder"))?;
    encoder::configure(&enc, p.encoder, p.video)?;
    force_keyframes_by_time(&enc, p.video.gop_seconds);

    let counters = Arc::new(Counters::default());

    let stats = pipeline.by_name("stats").ok_or_else(|| anyhow!("no stats"))?;
    let c = counters.clone();
    stats
        .static_pad("src")
        .unwrap()
        .add_probe(gst::PadProbeType::BUFFER, move |_, info| {
            if let Some(buf) = info.buffer() {
                c.frames.fetch_add(1, Ordering::Relaxed);
                c.bytes.fetch_add(buf.size() as u64, Ordering::Relaxed);
            }
            gst::PadProbeReturn::Ok
        });

    // A leaky queue that overruns has just dropped a frame. Raw frames before
    // the encoder can be dropped freely; encoded ones need the keyframe guard.
    for q in ["capq", "encq"] {
        if let Some(queue) = pipeline.by_name(q) {
            let c = counters.clone();
            queue.connect("overrun", false, move |_| {
                c.dropped.fetch_add(1, Ordering::Relaxed);
                None
            });
        }
    }
    let q_rtsp = pipeline.by_name("q_rtsp").ok_or_else(|| anyhow!("no q_rtsp"))?;
    guard_encoded_queue(&q_rtsp, counters.clone());

    Ok(StreamPipeline { pipeline, counters })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mutter_source_requests_size() {
        let v = VideoSettings::default();
        let s = source_description(
            &CaptureSource::PipeWire { fd: None, node_id: 42, drives_size: true },
            &v,
        );
        assert!(s.starts_with("pipewiresrc path=42"));
        assert!(s.contains("width=1920,height=1080"));
        assert!(!s.contains(" fd="));
    }

    #[test]
    fn x11_region_is_inclusive() {
        let v = VideoSettings { width: 1280, height: 720, ..Default::default() };
        let s = source_description(
            &CaptureSource::X11 { display: ":0".into(), x: 1920, y: 0, width: 1280, height: 720 },
            &v,
        );
        assert!(s.contains("startx=1920 starty=0 endx=3199 endy=719"));
    }

    /// Every element a pipeline can contain must be declared in
    /// packaging/deps.toml, so the installer knows to provide it.
    #[test]
    fn all_elements_declared_in_deps_manifest() {
        let manifest: toml::Value =
            toml::from_str(include_str!("../../../packaging/deps.toml")).expect("deps.toml parses");
        let mut declared = std::collections::BTreeSet::new();
        for dep in manifest["dep"].as_array().unwrap() {
            let probe = &dep["probe"];
            if probe["kind"].as_str() == Some("gst-element") {
                for key in ["elements", "covers"] {
                    if let Some(list) = probe.get(key).and_then(|v| v.as_array()) {
                        declared.extend(list.iter().map(|e| e.as_str().unwrap().to_string()));
                    }
                }
            }
        }

        let mut used = std::collections::BTreeSet::new();
        let sources = [
            CaptureSource::PipeWire { fd: None, node_id: 1, drives_size: true },
            CaptureSource::X11 { display: ":0".into(), x: 0, y: 0, width: 640, height: 360 },
            CaptureSource::Test,
        ];
        for cand in encoder::CANDIDATES {
            used.insert(cand.element.to_string());
            let v = VideoSettings { codec: cand.codec, ..Default::default() };
            for src in &sources {
                let p = PipelineParams {
                    video: &v,
                    encoder: cand.element,
                    source: src,
                    rtsp_publish: "rtsp://127.0.0.1:1/x".into(),
                };
                for segment in description(&p).split('!') {
                    let el = segment.split_whitespace().next().unwrap_or_default();
                    if !el.is_empty() && !el.contains('/') && !el.ends_with('.') {
                        used.insert(el.to_string());
                    }
                }
            }
        }
        // Elements rtspclientsink loads internally, and the encoder probe's.
        used.extend(["rtph264pay", "rtph265pay", "rtpbin", "capsfilter", "fakesink"].map(String::from));

        let missing: Vec<_> = used.difference(&declared).collect();
        assert!(missing.is_empty(), "add these to packaging/deps.toml: {missing:?}");
    }

    #[test]
    fn full_description_parses() {
        gst::init().unwrap();
        let v = VideoSettings { width: 640, height: 360, fps: 30, ..Default::default() };
        let p = PipelineParams {
            video: &v,
            encoder: "x264enc",
            source: &CaptureSource::Test,
            rtsp_publish: "rtsp://127.0.0.1:1/x".into(),
        };
        let d = description(&p);
        assert!(d.contains("h264parse config-interval=-1"));
        assert!(d.contains("colorimetry=bt709"));
        assert!(d.contains("! video/x-h264,profile=(string){high,main,constrained-baseline} !"));
        if gst::ElementFactory::find("x264enc").is_some()
            && gst::ElementFactory::find("rtspclientsink").is_some()
        {
            let sp = build(&p).expect("pipeline builds");
            let enc = sp.pipeline.by_name("enc").unwrap();
            assert_eq!(enc.property::<u32>("bframes"), 0);
            assert_eq!(enc.property::<u32>("key-int-max"), 30);
        }
    }

    /// Encode into a leaky queue drained slower than the frame rate, so it
    /// keeps dropping. Returns (overruns, output frames, broken frames): a
    /// frame is broken when it's a delta frame whose predecessor never came
    /// out, i.e. a receiver would decode garbage.
    fn run_leaky(guard: bool) -> (u64, usize, usize) {
        use std::sync::Mutex;
        let pipeline = gst::parse::launch(
            "videotestsrc is-live=true num-buffers=90 ! video/x-raw,width=320,height=240,framerate=30/1 \
             ! x264enc tune=zerolatency speed-preset=ultrafast key-int-max=15 bframes=0 ! h264parse \
             ! queue name=q max-size-buffers=3 max-size-bytes=0 max-size-time=0 leaky=downstream \
             ! identity sleep-time=60000 ! fakesink name=sink sync=false",
        )
        .unwrap()
        .downcast::<gst::Pipeline>()
        .unwrap();
        let q = pipeline.by_name("q").unwrap();
        let counters = Arc::new(Counters::default());
        if guard {
            guard_encoded_queue(&q, counters.clone());
        } else {
            let c = counters.clone();
            q.connect("overrun", false, move |_| {
                c.dropped.fetch_add(1, Ordering::Relaxed);
                None
            });
        }
        let record = |pad: gst::Pad| {
            let log = Arc::new(Mutex::new(Vec::<(gst::ClockTime, bool)>::new()));
            let l = log.clone();
            pad.add_probe(gst::PadProbeType::BUFFER, move |_, info| {
                if let Some(b) = info.buffer() {
                    let delta = b.flags().contains(gst::BufferFlags::DELTA_UNIT);
                    l.lock().unwrap().push((b.pts().unwrap(), delta));
                }
                gst::PadProbeReturn::Ok
            });
            log
        };
        let input = record(q.static_pad("sink").unwrap());
        let output = record(pipeline.by_name("sink").unwrap().static_pad("sink").unwrap());

        pipeline.set_state(gst::State::Playing).unwrap();
        let msg = pipeline
            .bus()
            .unwrap()
            .timed_pop_filtered(gst::ClockTime::from_seconds(20), &[gst::MessageType::Eos, gst::MessageType::Error]);
        pipeline.set_state(gst::State::Null).unwrap();
        assert!(matches!(msg.map(|m| m.type_()), Some(gst::MessageType::Eos)), "pipeline didn't finish");

        let input = input.lock().unwrap();
        let output = output.lock().unwrap();
        let out_pts: std::collections::HashSet<_> = output.iter().map(|f| f.0).collect();
        let broken = output
            .iter()
            .filter(|(pts, delta)| {
                let i = input.iter().position(|f| f.0 == *pts).unwrap();
                *delta && (i == 0 || !out_pts.contains(&input[i - 1].0))
            })
            .count();
        (counters.dropped.load(Ordering::Relaxed), output.len(), broken)
    }

    #[test]
    fn leaky_encoded_queue_never_emits_broken_frames() {
        gst::init().unwrap();
        if gst::ElementFactory::find("x264enc").is_none() {
            eprintln!("x264enc not installed; skipping");
            return;
        }
        // Without the guard the test setup really does break the stream...
        let (overruns, _, broken) = run_leaky(false);
        assert!(overruns > 0 && broken > 0, "test setup didn't drop frames: {overruns} {broken}");
        // ...with it, nothing undecodable comes out, and video still flows.
        let (overruns, frames, broken) = run_leaky(true);
        assert!(overruns > 0, "no overruns");
        assert_eq!(broken, 0, "broken frames reached the output");
        assert!(frames > 5, "guard starved the output: {frames} frames");
    }
}
