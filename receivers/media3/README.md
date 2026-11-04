# CastDesk Media3 receiver (placeholder)

> **Status: stub (stage c).** There is no code here yet.

Planned: a small Android / Android TV app built on AndroidX Media3
(ExoPlayer) that

- plays a CastDesk RTSP stream with low-latency settings (and SRT/UDP once
  stage (b) adds those destinations),
- speaks the WebSocket control protocol in
  [`docs/control-protocol.md`](../../docs/control-protocol.md), so castdeskd
  can tell it what to play and it can report status back.

Until then, any RTSP-capable player works as a receiver: VLC, mpv, ffplay,
or a stock Android TV player app.
