#!/usr/bin/env bash
# Headless end-to-end check for stage (a): no desktop needed.
# Starts an isolated castdeskd (own config, own ports) with a test-pattern
# display, then verifies the RTSP stream the way a receiver would.
#
#   scripts/selftest-stage-a.sh [path/to/mediamtx]
#   ENCODER=software scripts/selftest-stage-a.sh   # force a family (auto, vaapi, nvenc, software)
#
# What it does NOT cover: any real virtual-display backend (it uses
# backend = "test"), a specific encoder (it uses whatever `auto` picks:
# x264enc on a machine without a GPU),
# the TUI, receivers other than ffprobe/ffmpeg/curl, or WebRTC media (only
# the player page is fetched; there is no headless WebRTC client here).
set -euo pipefail
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
MTX="${1:-$(command -v mediamtx || echo "${XDG_DATA_HOME:-$HOME/.local/share}/castdesk/bin/mediamtx")}"
[[ -x "$MTX" ]] || { echo "mediamtx not found (pass its path)"; exit 1; }
command -v ffprobe >/dev/null || { echo "ffprobe needed (apt install ffmpeg)"; exit 1; }

WORK="$(mktemp -d)"
trap 'kill $PID 2>/dev/null; wait $PID 2>/dev/null; rm -rf "$WORK"' EXIT
# Non-default ports so this can run next to your real daemon.
cat > "$WORK/config.toml" <<EOF
[daemon]
listen = "127.0.0.1:17650"
[mediamtx]
binary = "$MTX"
rtsp_port = 18554
api_port = 19997
rtp_port = 18000
rtcp_port = 18001
hls = true
hls_port = 18888
webrtc = true
webrtc_port = 18889
webrtc_udp_port = 18189
EOF
export CASTDESK_CONFIG="$WORK/config.toml" CASTDESK_RUNTIME_DIR="$WORK/run"
# Terminals inside snap apps (e.g. VS Code installed as a snap) export
# GIO_MODULE_DIR and friends pointing at the snap's own libraries. GStreamer's
# network elements then load them and the daemon dies with
# "symbol lookup error: /snap/core20/.../libpthread.so.0".
unset GIO_MODULE_DIR GTK_PATH GTK_EXE_PREFIX LOCPATH GSETTINGS_SCHEMA_DIR \
      GDK_PIXBUF_MODULEDIR GDK_PIXBUF_MODULE_FILE GTK_IM_MODULE_FILE

(cd "$ROOT" && cargo build -q -p castdeskd)
"$ROOT/target/debug/castdeskd" run >"$WORK/log" 2>&1 &
PID=$!
for _ in $(seq 50); do [[ -s "$WORK/run/token" ]] && break; sleep 0.2; done
H="Authorization: Bearer $(cat "$WORK/run/token")"
API=http://127.0.0.1:17650/api/v1
pass() { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
fail() { { printf '  \033[31mFAIL\033[0m %s\n' "$*"; tail -30 "$WORK/log"; } >&2; exit 1; }
# Display state, or fail with the daemon log if the API stops answering.
state() {
  local body
  body=$(curl -s -H "$H" $API/displays/selftest) || true
  [[ -n $body ]] || { kill -0 $PID 2>/dev/null || fail "castdeskd died"; fail "API returned nothing"; }
  python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])' <<<"$body"
}

code=$(curl -s -o /dev/null -w '%{http_code}' $API/displays)
[[ $code == 401 ]] && pass "API rejects requests without token" || fail "expected 401, got $code"

curl -sf -H "$H" -H 'content-type: application/json' -X POST $API/displays \
  -d '{"name":"selftest","backend":"test","encoder":"'"${ENCODER:-auto}"'","width":1280,"height":720,"fps":30,"bitrate_kbps":4000}' >/dev/null \
  && pass "created test display" || fail "create display"

for _ in $(seq 40); do
  st=$(state)
  [[ $st == running ]] && break; sleep 0.25
done
[[ $st == running ]] && pass "pipeline running" || fail "state=$st"

out=$(timeout 15 ffprobe -v error -rtsp_transport tcp -show_entries stream=codec_name,width,height,has_b_frames \
  -of csv=p=0 rtsp://127.0.0.1:18554/selftest)
[[ $out == "h264,1280,720,0" ]] && pass "RTSP/TCP: H.264 1280x720, no B-frames" || fail "ffprobe: $out"

# What a TV decoder cares about: a profile/level it supports and 8-bit 4:2:0.
out=$(timeout 15 ffprobe -v error -rtsp_transport tcp \
  -show_entries stream=profile,level,pix_fmt,color_space,color_range \
  -of json rtsp://127.0.0.1:18554/selftest)
python3 - "$out" <<'EOF' && pass "RTSP: TV-safe profile/level, yuv420p" || fail "ffprobe: $out"
import json, sys
s = json.loads(sys.argv[1])["streams"][0]
assert s["profile"] in ("High", "Main", "Constrained Baseline"), s
assert 0 < s["level"] <= 42, s  # 4.2 = what 1080p60 TVs decode
assert s["pix_fmt"] == "yuv420p", s
EOF
# Colour tags: wrong ones fail, missing ones only warn. x264enc writes the
# BT.709 tags the pipeline asks for; vah264enc (GStreamer 1.28) writes none,
# and players then assume BT.709 for HD sizes, which is what we send.
enc=$(curl -s -H "$H" $API/displays/selftest | python3 -c 'import json,sys;print(json.load(sys.stdin)["encoder"])')
colour=$(python3 -c 'import json,sys;s=json.loads(sys.argv[1])["streams"][0];print(s.get("color_space","unknown"),s.get("color_range","unknown"))' "$out")
case $colour in
  "bt709 tv") pass "RTSP: tagged BT.709 limited range ($enc)" ;;
  "unknown unknown") printf '  \033[33mWARN\033[0m %s doesn'"'"'t tag colour; players will assume BT.709 for HD\n' "$enc" ;;
  *) fail "colour tags wrong for $enc: $colour" ;;
esac

timeout 15 ffmpeg -v error -rtsp_transport udp -i rtsp://127.0.0.1:18554/selftest -t 3 -f null - \
  && pass "RTSP/UDP: 3 s decoded cleanly" || fail "UDP decode"

# HLS needs a couple of segments (1 s keyframes) before the playlist exists.
for _ in $(seq 30); do
  curl -sf -o /dev/null http://127.0.0.1:18888/selftest/index.m3u8 && break; sleep 0.5
done
out=$(timeout 20 ffprobe -v error -select_streams v:0 -show_entries stream=codec_name,width,height \
  -of csv=p=0 http://127.0.0.1:18888/selftest/index.m3u8 | head -1) || true
[[ $out == "h264,1280,720" ]] && pass "HLS: H.264 1280x720" || fail "HLS ffprobe: $out"

code=$(curl -s -o /dev/null -w '%{http_code}' http://127.0.0.1:18889/selftest/)
[[ $code == 200 ]] && pass "WebRTC player page served" || fail "WebRTC page: HTTP $code"

urls=$(curl -s -H "$H" $API/displays/selftest | python3 -c 'import json,sys;print(" ".join(u["kind"] for u in json.load(sys.stdin)["urls"]))')
[[ $urls == "rtsp hls webrtc" ]] && pass "API advertises RTSP, HLS and WebRTC URLs" || fail "urls: $urls"

sleep 1.5
stats=$(curl -s -H "$H" $API/displays/selftest)
python3 - "$stats" <<'EOF' && pass "stats: bitrate and fps within 25% of target" || fail "stats off: $stats"
import json, sys
s = json.loads(sys.argv[1])["stats"]
assert 3000 < s["bitrate_kbps"] < 5000 and 22 < s["fps"] < 38, s
EOF

pkill -f -- "$MTX $WORK" || true
sleep 6
st=$(state)
[[ $st == running ]] && pass "recovered after MediaMTX was killed" || fail "after MediaMTX kill: $st"

curl -sf -H "$H" -X DELETE $API/displays/selftest >/dev/null && pass "removed display" || fail "delete"
grep -q selftest "$WORK/config.toml" && fail "display still in config" || pass "config updated"
echo "stage (a) self-test OK"
