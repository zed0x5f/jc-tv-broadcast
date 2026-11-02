#!/usr/bin/env bash
# Headless end-to-end check for stage (a): no desktop needed.
# Starts an isolated castdeskd (own config, own ports) with a test-pattern
# display, then verifies the RTSP stream the way a receiver would.
#
#   scripts/selftest-stage-a.sh [path/to/mediamtx]
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
EOF
export CASTDESK_CONFIG="$WORK/config.toml" CASTDESK_RUNTIME_DIR="$WORK/run"

(cd "$ROOT" && cargo build -q -p castdeskd)
"$ROOT/target/debug/castdeskd" run >"$WORK/log" 2>&1 &
PID=$!
for _ in $(seq 50); do [[ -s "$WORK/run/token" ]] && break; sleep 0.2; done
H="Authorization: Bearer $(cat "$WORK/run/token")"
API=http://127.0.0.1:17650/api/v1
pass() { printf '  \033[32mPASS\033[0m %s\n' "$*"; }
fail() { printf '  \033[31mFAIL\033[0m %s\n' "$*"; tail -30 "$WORK/log"; exit 1; }

code=$(curl -s -o /dev/null -w '%{http_code}' $API/displays)
[[ $code == 401 ]] && pass "API rejects requests without token" || fail "expected 401, got $code"

curl -sf -H "$H" -H 'content-type: application/json' -X POST $API/displays \
  -d '{"name":"selftest","backend":"test","width":1280,"height":720,"fps":30,"bitrate_kbps":4000}' >/dev/null \
  && pass "created test display" || fail "create display"

for _ in $(seq 40); do
  st=$(curl -s -H "$H" $API/displays/selftest | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')
  [[ $st == running ]] && break; sleep 0.25
done
[[ $st == running ]] && pass "pipeline running" || fail "state=$st"

out=$(timeout 15 ffprobe -v error -rtsp_transport tcp -show_entries stream=codec_name,width,height,has_b_frames \
  -of csv=p=0 rtsp://127.0.0.1:18554/selftest)
[[ $out == "h264,1280,720,0" ]] && pass "RTSP/TCP: H.264 1280x720, no B-frames" || fail "ffprobe: $out"

timeout 15 ffmpeg -v error -rtsp_transport udp -i rtsp://127.0.0.1:18554/selftest -t 3 -f null - \
  && pass "RTSP/UDP: 3 s decoded cleanly" || fail "UDP decode"

sleep 1.5
stats=$(curl -s -H "$H" $API/displays/selftest)
python3 - "$stats" <<'EOF' && pass "stats: bitrate and fps within 25% of target" || fail "stats off: $stats"
import json, sys
s = json.loads(sys.argv[1])["stats"]
assert 3000 < s["bitrate_kbps"] < 5000 and 22 < s["fps"] < 38, s
EOF

pkill -f -- "$MTX $WORK" || true
sleep 6
st=$(curl -s -H "$H" $API/displays/selftest | python3 -c 'import json,sys;print(json.load(sys.stdin)["state"])')
[[ $st == running ]] && pass "recovered after MediaMTX was killed" || fail "after MediaMTX kill: $st"

curl -sf -H "$H" -X DELETE $API/displays/selftest >/dev/null && pass "removed display" || fail "delete"
grep -q selftest "$WORK/config.toml" && fail "display still in config" || pass "config updated"
echo "stage (a) self-test OK"
