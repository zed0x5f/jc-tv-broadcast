#!/usr/bin/env bash
# CastDesk installer (per-user; nothing here needs root unless you pass --deps).
#
#   packaging/install.sh            build + install binaries, MediaMTX, systemd user unit
#   packaging/install.sh --deps     also install distro packages (uses sudo)
#   packaging/install.sh --no-mediamtx   skip downloading MediaMTX (use your own)
#   packaging/install.sh --uninstall
set -euo pipefail

MEDIAMTX_VERSION="v1.21.3"
PREFIX="${HOME}/.local"
BIN="${PREFIX}/bin"
DATA="${XDG_DATA_HOME:-$HOME/.local/share}/castdesk"
UNIT_DIR="${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user"
APPS="${XDG_DATA_HOME:-$HOME/.local/share}/applications"
ROOT="$(cd "$(dirname "$0")/.." && pwd)"

want_deps=0 want_mtx=1 uninstall=0
for a in "$@"; do
  case "$a" in
    --deps) want_deps=1 ;;
    --no-mediamtx) want_mtx=0 ;;
    --uninstall) uninstall=1 ;;
    -h|--help) sed -n 2,8p "$0"; exit 0 ;;
    *) echo "unknown option $a" >&2; exit 2 ;;
  esac
done

say() { printf '\033[1;36m==>\033[0m %s\n' "$*"; }
warn() { printf '\033[1;33m!!\033[0m %s\n' "$*" >&2; }

if [[ $uninstall == 1 ]]; then
  systemctl --user disable --now castdeskd.service 2>/dev/null || true
  rm -f "$BIN/castdeskd" "$BIN/castdesk-tui" "$UNIT_DIR/castdeskd.service" "$APPS/castdesk-tui.desktop"
  rm -rf "$DATA/bin"
  systemctl --user daemon-reload || true
  say "removed. Config kept in ~/.config/castdesk (delete it yourself if you want)."
  exit 0
fi

install_deps() {
  . /etc/os-release
  case "${ID} ${ID_LIKE:-}" in
    *ubuntu*|*debian*)
      say "installing packages with apt (sudo)"
      sudo apt-get install -y build-essential pkg-config curl \
        libgstreamer1.0-dev libgstreamer-plugins-base1.0-dev \
        gstreamer1.0-plugins-base gstreamer1.0-plugins-good gstreamer1.0-plugins-bad \
        gstreamer1.0-plugins-ugly gstreamer1.0-pipewire gstreamer1.0-rtsp gstreamer1.0-tools \
        xdg-desktop-portal x11-xserver-utils wl-clipboard
      warn "Intel: also 'sudo apt install intel-media-va-driver-non-free'. AMD: mesa-va-drivers. Check with 'vainfo'."
      ;;
    *fedora*|*rhel*)
      say "installing packages with dnf (sudo)"
      sudo dnf install -y gcc pkgconf-pkg-config curl \
        gstreamer1-devel gstreamer1-plugins-base-devel gstreamer1-plugins-good \
        gstreamer1-plugins-bad-free gstreamer1-rtsp-server pipewire-gstreamer \
        xdg-desktop-portal xrandr wl-clipboard
      warn "Fedora: x264 and AMD/Intel H.264 VA-API encode need RPM Fusion:"
      warn "  sudo dnf install gstreamer1-plugins-ugly mesa-va-drivers-freeworld   (AMD)"
      warn "  sudo dnf install gstreamer1-plugins-ugly intel-media-driver          (Intel)"
      ;;
    *arch*)
      sudo pacman -S --needed base-devel gstreamer gst-plugins-base gst-plugins-good \
        gst-plugins-bad gst-plugins-ugly gst-plugin-pipewire gst-rtsp-server \
        xdg-desktop-portal xorg-xrandr wl-clipboard
      ;;
    *) warn "unknown distro ${ID}; install GStreamer >= 1.22 dev packages + plugins manually" ;;
  esac
}

[[ $want_deps == 1 ]] && install_deps

command -v cargo >/dev/null || { warn "cargo not found: install Rust from https://rustup.rs"; exit 1; }
pkg-config --exists gstreamer-1.0 || { warn "GStreamer dev files missing; rerun with --deps"; exit 1; }

say "building release binaries"
(cd "$ROOT" && cargo build --release --locked -p castdeskd -p castdesk-tui)
mkdir -p "$BIN"
install -m755 "$ROOT/target/release/castdeskd" "$ROOT/target/release/castdesk-tui" "$BIN/"

if [[ $want_mtx == 1 ]]; then
  if [[ -x "$DATA/bin/mediamtx" ]] && [[ "$("$DATA/bin/mediamtx" --version 2>/dev/null)" == "$MEDIAMTX_VERSION" ]]; then
    say "MediaMTX $MEDIAMTX_VERSION already installed"
  else
    case "$(uname -m)" in
      x86_64) arch=amd64 ;;
      aarch64) arch=arm64 ;;
      armv7l) arch=armv7 ;;
      *) warn "no MediaMTX build for $(uname -m)"; arch="" ;;
    esac
    if [[ -n $arch ]]; then
      tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
      base="https://github.com/bluenviron/mediamtx/releases/download/${MEDIAMTX_VERSION}"
      tarball="mediamtx_${MEDIAMTX_VERSION}_linux_${arch}.tar.gz"
      say "downloading MediaMTX $MEDIAMTX_VERSION ($arch)"
      curl -fsSL -o "$tmp/$tarball" "$base/$tarball"
      curl -fsSL -o "$tmp/checksums.sha256" "$base/checksums.sha256"
      (cd "$tmp" && grep " $tarball\$" checksums.sha256 | sha256sum -c -) \
        || { warn "MediaMTX checksum mismatch — not installing"; exit 1; }
      tar -xzf "$tmp/$tarball" -C "$tmp" mediamtx
      mkdir -p "$DATA/bin"
      install -m755 "$tmp/mediamtx" "$DATA/bin/mediamtx"
      say "MediaMTX installed to $DATA/bin/mediamtx"
    fi
  fi
fi

mkdir -p "$UNIT_DIR" "$APPS"
install -m644 "$ROOT/packaging/castdeskd.service" "$UNIT_DIR/"
install -m644 "$ROOT/packaging/castdesk-tui.desktop" "$APPS/"
systemctl --user daemon-reload
systemctl --user enable castdeskd.service

say "checking this machine"
"$BIN/castdeskd" doctor || true

case ":$PATH:" in *":$BIN:"*) ;; *) warn "$BIN is not on PATH; add it to your shell profile" ;; esac
cat <<EOF

Done. Next:
  systemctl --user start castdeskd     # starts now (and on every login)
  castdesk-tui                          # manage displays
Firewall (receivers on the LAN): allow TCP 8554 and UDP 8000-8001.
EOF
