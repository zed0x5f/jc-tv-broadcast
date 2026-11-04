//! One GStreamer pipeline per virtual display: capture → convert/scale →
//! encode once → parse → tee → destinations.
//!
//! Stage (a) has a single destination (RTSP publish to MediaMTX). The tee is
//! already in place so stage (b) adds SRT/UDP branches without touching the
//! encoder.

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

fn element_has_property(factory: &str, prop: &str) -> bool {
    gst::ElementFactory::make(factory)
        .build()
        .map(|e| e.find_property(prop).is_some())
        .unwrap_or(false)
}

/// The capture half of the pipeline, as gst-launch syntax.
pub fn source_description(src: &CaptureSource, v: &VideoSettings, pw_target_object: bool) -> String {
    match src {
        CaptureSource::PipeWire {
            node_id,
            drives_size,
            ..
        } => {
            let fd = src.raw_fd().map(|fd| format!(" fd={fd}")).unwrap_or_default();
            let target = if pw_target_object {
                format!("target-object={node_id}")
            } else {
                format!("path={node_id}")
            };
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
pub fn description(p: &PipelineParams<'_>, pw_target_object: bool) -> String {
    let v = p.video;
    let parser = encoder::parser_for(v.codec);
    format!(
        "{src} \
         ! queue name=capq max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream \
         ! videoconvert n-threads=4 ! videoscale \
         ! video/x-raw,format={fmt},width={w},height={h},pixel-aspect-ratio=1/1 \
         ! queue name=encq max-size-buffers=2 max-size-bytes=0 max-size-time=0 leaky=downstream \
         ! {enc} name=enc \
         ! {parser} config-interval=-1 \
         ! identity name=stats silent=true \
         ! tee name=t allow-not-linked=true \
         t. ! queue name=q_rtsp max-size-buffers=0 max-size-bytes=0 max-size-time=500000000 leaky=downstream \
            ! rtspclientsink name=rtsp location={url} protocols=tcp latency=0",
        src = source_description(p.source, v, pw_target_object),
        w = v.width,
        h = v.height,
        enc = p.encoder,
        fmt = encoder::input_format(p.encoder),
        url = p.rtsp_publish,
    )
}

pub fn build(p: &PipelineParams<'_>) -> Result<StreamPipeline> {
    let pw_target = matches!(p.source, CaptureSource::PipeWire { .. })
        && element_has_property("pipewiresrc", "target-object");
    let desc = description(p, pw_target);
    tracing::debug!(pipeline = %desc, "building");
    let pipeline = gst::parse::launch(&desc)
        .with_context(|| format!("parsing pipeline: {desc}"))?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;

    let enc = pipeline.by_name("enc").ok_or_else(|| anyhow!("no encoder"))?;
    encoder::configure(&enc, p.encoder, p.video);

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

    // A leaky queue that overruns has just dropped a frame.
    for q in ["capq", "encq"] {
        if let Some(queue) = pipeline.by_name(q) {
            let c = counters.clone();
            queue.connect("overrun", false, move |_| {
                c.dropped.fetch_add(1, Ordering::Relaxed);
                None
            });
        }
    }

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
            true,
        );
        assert!(s.starts_with("pipewiresrc target-object=42"));
        assert!(s.contains("width=1920,height=1080"));
        assert!(!s.contains(" fd="));
    }

    #[test]
    fn x11_region_is_inclusive() {
        let v = VideoSettings { width: 1280, height: 720, ..Default::default() };
        let s = source_description(
            &CaptureSource::X11 { display: ":0".into(), x: 1920, y: 0, width: 1280, height: 720 },
            &v,
            false,
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
                for segment in description(&p, true).split('!') {
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
        let d = description(&p, false);
        assert!(d.contains("h264parse config-interval=-1"));
        if gst::ElementFactory::find("x264enc").is_some()
            && gst::ElementFactory::find("rtspclientsink").is_some()
        {
            let sp = build(&p).expect("pipeline builds");
            let enc = sp.pipeline.by_name("enc").unwrap();
            assert_eq!(enc.property::<u32>("bframes"), 0);
            assert_eq!(enc.property::<u32>("key-int-max"), 30);
        }
    }
}
