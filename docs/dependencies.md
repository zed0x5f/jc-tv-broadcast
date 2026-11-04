# Dependency detection strategy

CastDesk's dependencies depend on the machine: a GNOME/Intel laptop needs
`xdg-desktop-portal-gnome` and the iHD VA driver, a Sway/NVIDIA one needs
`xdg-desktop-portal-wlr` and `libnvidia-encode-<driver version>`, and an X11
session needs neither. A hardcoded package list is wrong for almost everyone.
So installation works out the dependencies **at runtime, on the target
machine**:

```
packaging/deps.toml          WHAT is needed: one entry per dependency, with a probe,
                             conditions, and how to obtain it
packaging/castdesk-deps      HOW to check: detect environment → probe → resolve → install
packaging/install.sh         WHEN: build phase → build → runtime phase → final re-check
```

## 1. Detect the environment

| Fact | How | Why it matters |
|---|---|---|
| Distro, version, family | `/etc/os-release` (`ID`, `ID_LIKE`) | apt vs. other; which packages exist |
| Multiarch triplet | `dpkg-architecture -qDEB_HOST_MULTIARCH` | library/plugin paths (`/usr/lib/x86_64-linux-gnu/...`) |
| Kernel | `uname -r` | `linux-headers-<kernel>` for evdi |
| Session type + desktop | `XDG_SESSION_TYPE` / `XDG_CURRENT_DESKTOP` / `SWAYSOCK` / `HYPRLAND_INSTANCE_SIGNATURE`; **if absent (SSH, TTY), `loginctl show-session`** for your active graphical session | Wayland needs PipeWire + portal; which portal backend; X11 needs xrandr |
| GPUs + kernel driver | `/sys/class/drm/card*/device/vendor` (PCI ID) and `…/device/driver` symlink | VA-API driver (Intel/AMD), NVENC (NVIDIA proprietary only, not nouveau) |
| NVIDIA driver major | `/proc/driver/nvidia/version`, else `modinfo nvidia` | NVENC library is versioned per driver: `libnvidia-encode-570` |
| Session D-Bus / `$DISPLAY` | `$XDG_RUNTIME_DIR/bus`, `$DISPLAY` | some probes only work inside the session; otherwise they're reported *unverified*, not *missing* |

Detection mirrors `castdeskd`'s own session logic (`crates/castdeskd/src/session.rs`),
so the installer checks exactly the backend the daemon will use. Every
detected value can be overridden (`--session x11 --desktop KDE --gpu nvidia`),
for example to prepare a machine over SSH.

## 2. Probe each applicable dependency

Entries whose `when` conditions don't match are skipped (no Intel GPU, so no
iHD driver). Each remaining entry has a probe that tests the **capability**,
not merely "is a package installed":

| Probe | Checks | Example |
|---|---|---|
| `gst-element` | `gst-inspect-1.0 --exists <el>`; falls back to the plugin `.so` if gst-inspect isn't there yet | `pipewiresrc`, `vah264enc` (only registers once a VA driver works) |
| `toolchain` | every cargo on PATH, `~/.cargo/bin`, `/usr/lib/rust-*/bin`; picks the newest ≥ min that has a rustc | finds apt's `cargo-1.91` even though `/usr/bin/cargo` is 1.75 |
| `pkgconfig` | `pkg-config --modversion` ≥ min | `gstreamer-1.0 >= 1.22` |
| `version` | run a binary, regex its output, compare | MediaMTX ≥ 1.12, RandR ≥ 1.5, **installed** KDE portal ≥ 6 |
| `file` | glob (with `{multiarch}`, `{kernel}`, …) | `iHD_drv_video.so`, `gnome.portal` |
| `command` | exit 0 + optional regex | `vainfo --display drm` advertises `VAProfileH264…: VAEntrypointEncSlice` |
| `dbus-name` | `NameHasOwner` on the session bus | `org.gnome.Mutter.ScreenCast` |
| `access` | read/write on a device node | `/dev/dri/renderD*` (group membership) |
| `kmod` | `/sys/module/<m>` or `modinfo` | `evdi` |

Results are `ok`, `missing`, `unverified` (can't be checked from here: run
inside your desktop session), or `skipped` (doesn't apply).

## 3. Resolve what's missing to packages

For each missing entry, ask apt what it can actually deliver:

- **Candidates in order**: `["intel-media-va-driver-non-free", "intel-media-va-driver"]`.
  The first with an apt `Candidate:` wins. If the preferred one isn't
  available (multiverse disabled), the fallback is used and you're told why.
- **Templates** filled from the environment: `libnvidia-encode-{nvidia_major}`,
  `linux-headers-{kernel}`.
- **Versioned packages**: `{ versioned = "cargo-1.{minor}", min = "1.91" }`
  scans `apt-cache pkgnames` and picks the newest ≥ min, so Ubuntu 24.04 gets
  `cargo-1.91` + `rustc-1.91` from noble-updates and no rustup is needed.
- **Minimum candidate version**: `{ pkg = "xdg-desktop-portal-kde", min = "6.0" }`
  refuses to "fix" Kubuntu 24.04 by installing Plasma 5.27's portal, which
  can't make virtual monitors, and explains the alternatives.
- **Installed but still failing** (the package is there, the probe fails): it
  isn't reinstalled. You see the entry's `after` hint instead (re-login for
  group changes, reboot for a new NVIDIA driver, "elements register once a VA
  driver is present").
- **Not from apt** (`provider = download | driver | manual`): MediaMTX is
  downloaded by install.sh (pinned + sha256-verified); the NVIDIA driver
  points to `ubuntu-drivers install`; render-node access gets the `usermod`
  command.

## 4. Install, then re-probe

`--install` prints the exact `sudo apt-get install …` line, asks (unless
`--yes`), runs `apt-get update` if the lists are over a day old, installs,
and **runs every probe again**. Success means the capability now works,
not just that apt exited 0. Only `required` + `recommended` entries are
installed by default; `--optional` adds the rest.

`install.sh` runs this twice. First the build phase, so the build can't
fail on a missing `-dev` package. Then it builds with whichever toolchain
satisfied the probe, checks the runtime phase, downloads MediaMTX, and does a
final runtime check plus `castdeskd doctor`, which runs real encodes on each
encoder.

The checker is installed as `~/.local/bin/castdesk-deps`, so you can rerun it
any time something changes (new GPU, new driver, different desktop).

## 5. Keeping the manifest honest

Two tests fail if code and manifest drift apart:

- `cargo test -p castdeskd all_elements_declared_in_deps_manifest`: every
  GStreamer element any pipeline variant or encoder candidate can use must
  be declared in `deps.toml`. When it was added it immediately found six
  undeclared encoder elements.
- `python3 -m unittest discover -s packaging/tests`: every external program
  the Rust code runs (`Command::new("xrandr")`, `run("swaymsg", …)`, the
  clipboard tools) must be declared. The same suite covers detection,
  resolution and the full check against a fake GNOME/Intel laptop.

## Adding a dependency

Add a `[[dep]]` to `packaging/deps.toml` (the header documents every field),
then run both test suites. Find the right package on Ubuntu with
`dpkg -S <file>` if it's installed, or `apt-file search <file>` if not. For
GStreamer elements, `gst-inspect-1.0 <element> | grep Filename` gives the file.

## Limits

- Automatic installation is apt-only, because Ubuntu is the target. On other
  distros the checker still detects and probes everything, and lists the
  Ubuntu package names for you to translate.
- Session-specific probes need to run inside the graphical session. Over SSH
  you get `unverified` (and logind-based detection) rather than false
  failures.
- Hyprland isn't packaged for Ubuntu 24.04, so it's a manual step.
