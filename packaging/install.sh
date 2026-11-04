#!/usr/bin/env bash
# CastDesk installer. Run as your normal user from your desktop session;
# it calls sudo only to apt-get install packages it found missing.
#
#   packaging/install.sh                 detect deps → offer to install → build → install
#   packaging/install.sh --yes           same, without prompts
#   packaging/install.sh --check-only    only report what's missing (installs nothing)
#   packaging/install.sh --with-evdi     also check/install the evdi kernel module (needs root)
#   packaging/install.sh --no-mediamtx   don't download MediaMTX (bring your own)
#   packaging/install.sh --uninstall
#
# Dependencies are not hardcoded here: packaging/castdesk-deps probes this
# machine (session type, desktop, GPU/driver) against packaging/deps.toml
# and resolves what's missing to packages your apt sources can provide.
#
# --with-evdi only installs the kernel module. The evdi display backend in
# castdeskd is still a stub (stage b) and rejects `backend = "evdi"`.
#
# Testing: this script has never been run end to end by any automated test.
# The MediaMTX download + checksum step was run on its own against v1.21.3
# (it found and fixed the "*<file>" checksum-format bug). castdesk-deps has
# its own unit tests (packaging/tests), which run against faked machines.
set -euo pipefail

MEDIAMTX_VERSION="v1.21.3"
BIN="${HOME}/.local/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/castdesk"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

mode=install yes=0 want_mtx=1 uninstall=0 features=()
for a in "$@"; do
  case "$a" in
    --yes|-y) yes=1 ;;
    --deps) ;;                          # old flag: installing deps is now the default
    --check-only) mode=check ;;
    --with-evdi) features+=(--with evdi) ;;
    --no-mediamtx) want_mtx=0 ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n 2,14p "$0"; exit 0 ;;
    *) echo "unknown option $a (see --help)" >&2; exit 2 ;;
  esac
done

say()  { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }
die()  { printf '\033[1;31mxx\033[0m %s\n' "$*" >&2; exit 1; }

if [[ $uninstall == 1 ]]; then
  systemctl --user disable --now castdeskd.service 2>/dev/null || true
  rm -f "$BIN/castdeskd" "$BIN/castdesk-tui" "$BIN/castdesk-deps" \
        "$UNIT_DIR/castdeskd.service" "$APPS/castdesk-tui.desktop"
  rm -rf "$DATA/bin" "$DATA/deps.toml"
  systemctl --user daemon-reload 2>/dev/null || true
  say "removed. Config kept in ~/.config/castdesk (delete it yourself if you want)."
  exit 0
fi

[[ $EUID -eq 0 ]] && die "run as your normal user (not sudo): CastDesk is a per-user service; the installer calls sudo itself when it needs to"

# ── 0. bootstrap: the checker needs Python >= 3.11 (Ubuntu 24.04 has 3.12) ──
if ! python3 -c 'import sys; sys.exit(sys.version_info < (3, 11))' 2>/dev/null; then
  if command -v apt-get >/dev/null && [[ $mode == install ]]; then
    say "installing python3 (needed by the dependency checker)"
    sudo apt-get install -y python3
  else
    die "python3 >= 3.11 is required to check dependencies"
  fi
fi
DEPS=(python3 "$ROOT/packaging/castdesk-deps" --manifest "$ROOT/packaging/deps.toml")
install_args=()
if [[ $mode == install ]]; then
  install_args=(--install)
  [[ $yes == 1 ]] && install_args+=(--yes)
fi

# ── 1. build prerequisites ──
say "checking build prerequisites"
if ! "${DEPS[@]}" --phase build,install "${install_args[@]}"; then
  [[ $mode == check ]] && warn "build prerequisites missing (see above)" || die "build prerequisites still missing (see above)"
fi

# ── 2. runtime dependencies for this session + GPU ──
say "checking runtime dependencies for this machine"
runtime_ok=1
"${DEPS[@]}" --phase runtime "${features[@]}" "${install_args[@]}" || runtime_ok=0
[[ $mode == check ]] && exit $(( runtime_ok == 1 ? 0 : 1 ))

# ── 3. build with whichever toolchain satisfied the check ──
CARGO="$("${DEPS[@]}" which rust)" || die "no suitable Rust toolchain found"
export PATH="$(dirname "$CARGO"):$PATH"
say "building release binaries with $("$CARGO" --version)"
(cd "$ROOT" && "$CARGO" build --release --locked -p castdeskd -p castdesk-tui)
mkdir -p "$BIN" "$DATA"
install -m755 "$ROOT/target/release/castdeskd" "$ROOT/target/release/castdesk-tui" "$BIN/"
# Keep the checker installed so you can re-run it later (new GPU, new desktop…).
install -m755 "$ROOT/packaging/castdesk-deps" "$BIN/castdesk-deps"
install -m644 "$ROOT/packaging/deps.toml" "$DATA/deps.toml"

# ── 4. MediaMTX (not packaged by Ubuntu: pinned release, checksum-verified) ──
if [[ $want_mtx == 1 ]]; then
  if [[ -x "$DATA/bin/mediamtx" ]] && [[ "$("$DATA/bin/mediamtx" --version 2>/dev/null)" == "$MEDIAMTX_VERSION" ]]; then
    say "MediaMTX $MEDIAMTX_VERSION already installed"
  else
    case "$(uname -m)" in
      x86_64) arch=amd64 ;; aarch64) arch=arm64 ;; armv7l) arch=armv7 ;;
      *) die "no MediaMTX build for $(uname -m); install it yourself and rerun with --no-mediamtx" ;;
    esac
    tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
    base="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}"
    tarball="mediamtx_${MEDIAMTX_VERSION}_linux_${arch}.tar.gz"
    say "downloading MediaMTX $MEDIAMTX_VERSION ($arch)"
    curl -fsSL -o "$tmp/$tarball" "$base/$tarball"
    curl -fsSL -o "$tmp/checksums.sha256" "$base/checksums.sha256"
    # Upstream lists "<hash> *<file>" (binary-mode marker), so allow the '*'.
    (cd "$tmp" && grep -E " \*?${tarball//./\\.}\$" checksums.sha256 | sha256sum -c -) \
      || die "MediaMTX checksum mismatch, not installing"
    tar -xzf "$tmp/$tarball" -C "$tmp" mediamtx
    mkdir -p "$DATA/bin"
    install -m755 "$tmp/mediamtx" "$DATA/bin/mediamtx"
    say "MediaMTX installed to $DATA/bin/mediamtx"
  fi
fi

# ── 5. user service + launcher ──
mkdir -p "$UNIT_DIR" "$APPS"
install -m644 "$ROOT/packaging/castdeskd.service" "$UNIT_DIR/"
install -m644 "$ROOT/packaging/castdesk-tui.desktop" "$APPS/"
systemctl --user daemon-reload
systemctl --user enable castdeskd.service

# ── 6. final verification: deps re-check (MediaMTX now present) + real encoder probes ──
say "final check"
runtime_ok=1   # earlier result predates the MediaMTX download
"$BIN/castdesk-deps" --phase runtime "${features[@]}" || runtime_ok=0
"$BIN/castdeskd" doctor || true

case ":$PATH:" in *":$BIN:"*) ;; *) warn "$BIN is not on PATH; add it to your shell profile" ;; esac
[[ $runtime_ok == 1 ]] || warn "some runtime dependencies are still missing; fix them, then run: castdesk-deps"
cat <<EOF

Done. Next:
  systemctl --user start castdeskd     # starts now (and on every login)
  castdesk-tui                          # manage displays
  castdesk-deps                         # re-check dependencies any time
Firewall (receivers on the LAN): allow TCP 8554 and UDP 8000-8001.
EOF
