//! Hardware/software encoder discovery and low-latency configuration.
//!
//! Preference: VA-API (Intel/AMD) → NVENC (NVIDIA) → x264/x265. Every candidate
//! is probed with a tiny real encode at startup because "element exists" does
//! not imply "driver works" (missing VA driver, NVENC session limits, etc.).

use anyhow::{Context, Result, anyhow};
use castdesk_core::{Codec, EncoderInfo, EncoderPref, VideoSettings};
use gst::prelude::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Candidate {
    pub element: &'static str,
    pub codec: Codec,
    pub family: EncoderPref,
}

const fn c(element: &'static str, codec: Codec, family: EncoderPref) -> Candidate {
    Candidate {
        element,
        codec,
        family,
    }
}

/// In preference order.
pub const CANDIDATES: &[Candidate] = &[
    // GStreamer `va` plugin (maintained) first, then legacy gstreamer-vaapi.
    c("vah264enc", Codec::H264, EncoderPref::Vaapi),
    c("vah264lpenc", Codec::H264, EncoderPref::Vaapi),
    c("vaapih264enc", Codec::H264, EncoderPref::Vaapi),
    c("nvh264enc", Codec::H264, EncoderPref::Nvenc),
    c("x264enc", Codec::H264, EncoderPref::Software),
    c("vah265enc", Codec::H265, EncoderPref::Vaapi),
    c("vah265lpenc", Codec::H265, EncoderPref::Vaapi),
    c("vaapih265enc", Codec::H265, EncoderPref::Vaapi),
    c("nvh265enc", Codec::H265, EncoderPref::Nvenc),
    c("x265enc", Codec::H265, EncoderPref::Software),
];

/// Raw format the encoder should be fed (x265enc has no NV12 input).
pub fn input_format(element: &str) -> &'static str {
    match element {
        "x265enc" => "I420",
        _ => "NV12",
    }
}

pub fn parser_for(codec: Codec) -> &'static str {
    match codec {
        Codec::H264 => "h264parse",
        Codec::H265 => "h265parse",
    }
}

/// Low-latency properties per encoder. Each is applied only if the element
/// actually has that property, so plugin version differences don't break us.
pub fn low_latency_props(element: &str, v: &VideoSettings) -> Vec<(&'static str, String)> {
    let kbps = v.bitrate_kbps.to_string();
    let gop = v.gop_frames().to_string();
    match element {
        "vah264enc" | "vah264lpenc" | "vah265enc" | "vah265lpenc" => vec![
            ("rate-control", "cbr".into()),
            ("bitrate", kbps),
            ("key-int-max", gop),
            ("b-frames", "0".into()),
            ("ref-frames", "1".into()),
            ("target-usage", "6".into()),
            ("cpb-size", (v.bitrate_kbps / 2).to_string()), // ~0.5 s VBV
        ],
        "vaapih264enc" | "vaapih265enc" => vec![
            ("rate-control", "cbr".into()),
            ("bitrate", kbps),
            ("keyframe-period", gop),
            ("max-bframes", "0".into()),
            ("refs", "1".into()),
            ("quality-level", "6".into()),
        ],
        "nvh264enc" | "nvh265enc" => vec![
            ("preset", "low-latency-hq".into()),
            ("rc-mode", "cbr".into()),
            ("bitrate", kbps),
            ("gop-size", gop),
            ("bframes", "0".into()),
            ("zerolatency", "true".into()),
            ("rc-lookahead", "0".into()),
        ],
        "x264enc" => vec![
            ("tune", "zerolatency".into()),
            ("speed-preset", "superfast".into()),
            ("bitrate", kbps),
            ("key-int-max", gop),
            ("bframes", "0".into()),
            ("vbv-buf-capacity", "500".into()), // ms
            ("threads", "0".into()),
        ],
        "x265enc" => vec![
            ("tune", "zerolatency".into()),
            ("speed-preset", "superfast".into()),
            ("bitrate", kbps),
            ("key-int-max", gop),
        ],
        _ => vec![],
    }
}

/// Create an encoder element configured for low latency.
pub fn make_encoder(element: &str, v: &VideoSettings) -> Result<gst::Element> {
    let enc = gst::ElementFactory::make(element)
        .name("enc")
        .build()
        .with_context(|| format!("creating {element}"))?;
    configure(&enc, element, v);
    Ok(enc)
}

/// Apply low-latency settings to an existing encoder element.
pub fn configure(enc: &gst::Element, element: &str, v: &VideoSettings) {
    for (prop, value) in low_latency_props(element, v) {
        if enc.find_property(prop).is_some() {
            enc.set_property_from_str(prop, &value);
        } else {
            tracing::debug!(element, prop, "encoder lacks property, skipping");
        }
    }
}

/// Encode a few frames to prove the encoder works on this machine.
fn probe(cand: &Candidate) -> bool {
    if gst::ElementFactory::find(cand.element).is_none() {
        return false;
    }
    let run = || -> Result<()> {
        let pipeline = gst::Pipeline::new();
        let src = gst::ElementFactory::make("videotestsrc")
            .property("num-buffers", 5i32)
            .build()?;
        let caps = gst::ElementFactory::make("capsfilter")
            .property(
                "caps",
                gst::Caps::builder("video/x-raw")
                    .field("format", input_format(cand.element))
                    .field("width", 640i32)
                    .field("height", 360i32)
                    .field("framerate", gst::Fraction::new(30, 1))
                    .build(),
            )
            .build()?;
        let enc = make_encoder(cand.element, &VideoSettings {
            width: 640,
            height: 360,
            fps: 30,
            bitrate_kbps: 1000,
            ..Default::default()
        })?;
        let sink = gst::ElementFactory::make("fakesink").build()?;
        pipeline.add_many([&src, &caps, &enc, &sink])?;
        gst::Element::link_many([&src, &caps, &enc, &sink])?;
        pipeline.set_state(gst::State::Playing)?;
        let bus = pipeline.bus().unwrap();
        let res = match bus.timed_pop_filtered(
            gst::ClockTime::from_seconds(5),
            &[gst::MessageType::Eos, gst::MessageType::Error],
        ) {
            Some(m) => match m.view() {
                gst::MessageView::Eos(_) => Ok(()),
                gst::MessageView::Error(e) => Err(anyhow!("{}", e.error())),
                _ => unreachable!(),
            },
            None => Err(anyhow!("timeout")),
        };
        pipeline.set_state(gst::State::Null)?;
        res
    };
    match run() {
        Ok(()) => true,
        Err(e) => {
            tracing::info!(encoder = cand.element, "probe failed: {e:#}");
            false
        }
    }
}

/// Probe every candidate (blocking; call from spawn_blocking).
pub fn probe_all() -> Vec<EncoderInfo> {
    CANDIDATES
        .iter()
        .filter(|c| gst::ElementFactory::find(c.element).is_some())
        .map(|c| {
            let started = std::time::Instant::now();
            let working = probe(c);
            tracing::info!(
                encoder = c.element,
                working,
                ms = started.elapsed().as_millis() as u64,
                "encoder probe"
            );
            EncoderInfo {
                element: c.element.to_string(),
                codec: c.codec,
                family: c.family,
                working,
            }
        })
        .collect()
}

/// Pick the encoder for these settings from probe results.
pub fn choose(encoders: &[EncoderInfo], v: &VideoSettings) -> Result<String> {
    encoders
        .iter()
        .filter(|e| e.working && e.codec == v.codec)
        .find(|e| v.encoder == EncoderPref::Auto || e.family == v.encoder)
        .map(|e| e.element.clone())
        .ok_or_else(|| {
            anyhow!(
                "no working {} encoder for preference {:?} — run `castdeskd doctor`; VA-API needs gstreamer1.0-plugins-bad + a VA driver, x264 needs gstreamer1.0-plugins-ugly, x265 needs gstreamer1.0-plugins-bad",
                v.codec.label(),
                v.encoder
            )
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(el: &str, codec: Codec, family: EncoderPref, working: bool) -> EncoderInfo {
        EncoderInfo {
            element: el.into(),
            codec,
            family,
            working,
        }
    }

    #[test]
    fn auto_picks_first_working_hw() {
        let list = vec![
            info("vah264enc", Codec::H264, EncoderPref::Vaapi, false),
            info("nvh264enc", Codec::H264, EncoderPref::Nvenc, true),
            info("x264enc", Codec::H264, EncoderPref::Software, true),
        ];
        assert_eq!(choose(&list, &VideoSettings::default()).unwrap(), "nvh264enc");
        let sw = VideoSettings {
            encoder: EncoderPref::Software,
            ..Default::default()
        };
        assert_eq!(choose(&list, &sw).unwrap(), "x264enc");
        let h265 = VideoSettings {
            codec: Codec::H265,
            ..Default::default()
        };
        assert!(choose(&list, &h265).is_err());
    }

    #[test]
    fn every_candidate_disables_bframes_or_is_zerolatency() {
        let v = VideoSettings::default();
        for c in CANDIDATES {
            let props = low_latency_props(c.element, &v);
            let ok = props.iter().any(|(k, v)| {
                (k.contains("frames") || *k == "bframes") && v == "0" || *k == "tune"
            });
            assert!(ok, "{} lacks a no-B-frames / zerolatency setting", c.element);
            assert!(props.iter().any(|(k, v)| *k == "bitrate" && v == "12000"));
        }
    }

    #[test]
    fn x264_probe_works_when_installed() {
        gst::init().unwrap();
        if gst::ElementFactory::find("x264enc").is_none() {
            eprintln!("x264enc not installed; skipping");
            return;
        }
        assert!(probe(&CANDIDATES[4]));
    }
}
