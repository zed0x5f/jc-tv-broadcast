# CastDesk control protocol (draft, not implemented)

> **Status: stub (stage c).** Nothing in this document is implemented.
> `GET /api/v1/control` on castdeskd currently answers **501 Not Implemented**
> (`crates/castdeskd/src/remote/control_ws.rs`). Expect this draft to change.

The protocol lets the custom Media3 receiver app (`receivers/media3/`) and
castdeskd talk to each other over a WebSocket, so the laptop can act as a
remote and the receiver can report what it's doing.

## Open questions

- **Transport and auth.** The control API is loopback-only with a bearer
  token. Receivers are on the LAN, so this needs its own listener and a
  pairing step (e.g. a code shown on the TV, like Android TV Remote v2).
- **Discovery.** Receivers find the laptop via mDNS (`_castdesk._tcp`)?
- **Message format.** JSON text frames, one object per frame, with a
  `type` field. Sketch only:

```json
{ "type": "hello",  "role": "receiver", "version": 1, "name": "Living room TV" }
{ "type": "play",   "url": "rtsp://192.168.1.20:8554/tv1" }
{ "type": "key",    "key": "home" }
{ "type": "status", "playing": "tv1", "latency_ms": 180, "dropped": 3 }
```

## Related stubs

| Piece | File | Status |
|---|---|---|
| WebSocket endpoint | `crates/castdeskd/src/remote/control_ws.rs` | returns 501 |
| Android TV Remote v2 | `crates/castdeskd/src/remote/atv_remote.rs` | returns "not implemented" |
| ADB key events | `crates/castdeskd/src/remote/adb.rs` | returns "not implemented" |
| HDMI-CEC | `crates/castdeskd/src/remote/cec.rs` | returns "not implemented" |
| Media3 receiver app | `receivers/media3/` | placeholder README only |
