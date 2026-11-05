# Receiver compatibility

What CastDesk sends, what receivers need, and what has actually been tested.
Most "it doesn't play" reports come from one of three things: a codec or
level the decoder can't handle, blocked UDP, or a player without RTSP
support. This page covers each one.

## What the stream looks like

| | Value | Why |
|---|---|---|
| Codec | H.264 (default) or H.265 | H.264 plays everywhere. H.265 halves the bitrate, but browsers, many older TVs and some Android TV players can't decode it. |
| Profile | H.264: High, Main or Constrained Baseline (8-bit 4:2:0). H.265: Main. | Pinned in caps right after the encoder, so no encoder family can produce High 10 / 4:2:2 / 4:4:4, which hardware decoders often reject. |
| Level | Whatever the settings need. 1080p60 at 12 Mb/s fits **4.2**. | Validation rejects H.264 settings beyond level 5.2. Above 4.2 the TUI and daemon log show a warning, because many TVs and streaming sticks stop at 4.2. |
| B-frames | none | Lower latency; some RTSP clients mishandle reordering. |
| Keyframes | every `gop_seconds` of wall-clock time, SPS/PPS on each | A viewer can join mid-stream within about 1 s. |
| Colour | BT.709 limited range, requested in caps | x264enc writes the tags into the stream. **vah264enc (GStreamer 1.28) writes no colour tags**, so players assume BT.709 for HD sizes, which matches what we send. At SD sizes (below 720p) a player may assume BT.601, and colours shift slightly. |
| Frame rate | variable | A screencast only delivers frames when the screen changes, and a keepalive resends the last frame every 0.5 s. Encoding identical frames at full rate would cost CPU for nothing. RTP timestamps carry the real timing, and every player tested so far handles this. If a TV stutters on a static screen, report it: forcing a constant rate is a small change. |
| Audio | none yet (stage b) | When audio arrives it will be AAC: many TV RTSP clients can't play Opus. |

## Transport: use TCP on Wi-Fi

MediaMTX lets readers choose UDP, multicast or TCP. Most players try UDP
first. Two things go wrong with UDP:

- **Firewall.** UDP needs RTP/RTCP ports 8000–8001 open on the laptop. If
  they're blocked, the player hangs until it falls back to TCP (several
  seconds), or it never falls back at all. `castdeskd doctor` lists every port
  to open.
- **Wi-Fi loss.** At 12 Mb/s a lost UDP packet smears the picture until the
  next keyframe. TCP retransmits instead.

So on Wi-Fi, or whenever in doubt, tell the player to use TCP:

| Player | TCP option |
|---|---|
| ffplay / ffmpeg | `-rtsp_transport tcp` |
| mpv | `--rtsp-transport=tcp` |
| VLC | `--rtsp-tcp`, or Preferences → Input/Codecs → Live555 stream transport → "RTP over RTSP (TCP)" |
| GStreamer | `rtspsrc protocols=tcp` |
| Android (Media3/ExoPlayer) | `RtspMediaSource.Factory().setForceUseRtpTcp(true)` |

The TUI's copy-paste commands use TCP. On a wired LAN, UDP gives slightly
lower latency.

## Players without RTSP: HLS and WebRTC

Browsers can't play RTSP, and some smart-TV players and Kodi setups handle it
badly. MediaMTX can serve the same encode as HLS and/or WebRTC. It only
repackages the stream, so there is no extra encoding cost. Both are off by
default:

```toml
[mediamtx]
hls = true                  # http://<laptop>:8888/<name>/index.m3u8
hls_variant = "mpegts"      # mpegts (plays almost anywhere, ~3-4 s) | fmp4 | lowLatency (~1 s, newer players only)
webrtc = true               # http://<laptop>:8889/<name>/ (browser player page; WHEP at /<name>/whep)
```

Enabled outputs show up in the TUI and in the API's `urls` list. Ports to
open: HLS 8888/tcp; WebRTC 8889/tcp and 8189/udp (all configurable:
`hls_port`, `webrtc_port`, `webrtc_udp_port`).

## Compatibility matrix

✅ = played, checked by hand or by the self-test. ❔ = not tried yet. Please
add a row when you try a receiver.

| Receiver | RTSP/TCP | RTSP/UDP | HLS | WebRTC | Notes |
|---|---|---|---|---|---|
| ffprobe / ffmpeg (self-test) | ✅ | ✅ | ✅ | — | Self-test also checks profile, level ≤ 4.2, yuv420p, colour tags. |
| GStreamer rtspsrc + vah264dec | ✅ | ✅ | ❔ | — | Played over the LAN address on GNOME; which transport was used wasn't recorded, so the two columns share one result. |
| Browser (Chrome/Firefox) | — | — | ❔ | ❔ (page served, media not tested) | No headless WebRTC client in the self-test. |
| mpv | ❔ | ❔ | ❔ | — | |
| VLC (snap/Flatpak, with live555) | ❔ | ❔ | ❔ | — | Debian/Ubuntu VLC builds lack RTSP support: see README troubleshooting. |
| Android TV (Media3 / any RTSP app) | ❔ | ❔ | ❔ | ❔ | |
| Kodi | ❔ | ❔ | ❔ | — | |
| Raspberry Pi (mpv / GStreamer) | ❔ | ❔ | ❔ | — | |
| Smart-TV built-in player | ❔ | ❔ | ❔ | ❔ | |

Encoders: the self-test passes with `vah264enc` (Intel) and `x264enc`
(`ENCODER=software scripts/selftest-stage-a.sh`). NVENC and AMD VA-API haven't
been tested.
