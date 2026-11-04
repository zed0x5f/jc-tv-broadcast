"""Tests for packaging/castdesk-deps. Run: python3 -m unittest discover -s packaging/tests"""

import importlib.machinery
import importlib.util
import re
import sys
import types
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
_loader = importlib.machinery.SourceFileLoader("castdesk_deps", str(ROOT / "packaging/castdesk-deps"))
_spec = importlib.util.spec_from_loader("castdesk_deps", _loader)
cd = importlib.util.module_from_spec(_spec)
sys.modules["castdesk_deps"] = cd   # dataclasses look the module up while executing it
_loader.exec_module(cd)

MANIFEST = ROOT / "packaging/deps.toml"


class FakeSystem(cd.System):
    """A scripted machine: commands → (rc, output), files, executables."""

    def __init__(self, commands=None, files=None, bins=None, environ=None, links=None, rw=True):
        self.commands = commands or {}
        self.files = files or {}          # path -> content (None = exists, empty)
        self.bins = set(bins or [])
        self._environ = environ or {}
        self.links = links or {}
        self.rw = rw
        self.calls = []

    def run(self, cmd, timeout=20):
        self.calls.append(cmd)
        key = " ".join(cmd)
        for pattern, result in self.commands.items():
            if re.fullmatch(pattern, key):
                return result
        return 127 if cmd[0] not in self.bins and "/" not in cmd[0] else 1, ""

    def which(self, name):
        if name in self.bins:
            return name if "/" in name else f"/usr/bin/{name}"
        if name.startswith("/usr/bin/") and name[len("/usr/bin/"):] in self.bins:
            return name
        return None

    def glob(self, pattern):
        rx = re.compile("^" + re.escape(pattern).replace(r"\*", "[^/]*").replace(r"\[0\-9\]", "[0-9]") + "$")
        return sorted(p for p in self.files if rx.match(p))

    def access_rw(self, path):
        return self.rw

    def read(self, path):
        return self.files.get(path)

    def readlink(self, path):
        return self.links.get(path)

    def environ(self):
        return dict(self._environ)


def args(**kw):
    base = dict(session=None, desktop=None, gpu=None, with_=None)
    base.update(kw)
    return types.SimpleNamespace(**base)


UBUNTU = {"/etc/os-release": 'ID=ubuntu\nVERSION_ID="24.04"\nID_LIKE=debian\n'}


class ManifestTests(unittest.TestCase):
    def test_manifest_is_valid(self):
        deps = cd.load_manifest(MANIFEST)
        self.assertGreater(len(deps), 30)

    def test_rust_commands_are_declared(self):
        """Every external program the Rust code runs must be in deps.toml."""
        deps = cd.load_manifest(MANIFEST)
        declared = set()
        for d in deps:
            p = d["probe"]
            declared.update(filter(None, [p.get("name"), p.get("also_binary"), p.get("requires_binary")]))
            declared.update(p.get("also", []) + p.get("also_any", []) + p.get("bins", []))
            if p.get("cmd"):
                declared.add(p["cmd"][0])
        used = set()
        for rs in (ROOT / "crates").rglob("*.rs"):
            text = rs.read_text()
            used.update(re.findall(r'Command::new\("([\w.-]+)"\)', text))
            used.update(re.findall(r'\brun\("([\w.-]+)"', text))
            used.update(re.findall(r'\("([a-z][\w-]*)", &\[', text))   # clipboard tool table
        missing = used - declared
        self.assertFalse(missing, f"add to packaging/deps.toml: {sorted(missing)}")

    def test_bad_manifest_rejected(self):
        tmp = ROOT / "target" / "bad-deps.toml"
        tmp.parent.mkdir(exist_ok=True)
        tmp.write_text('[[dep]]\nid="x"\nphase="sometime"\nlevel="required"\nwhat="x"\nprobe={kind="nope"}\n')
        with self.assertRaises(ValueError) as e:
            cd.load_manifest(tmp)
        self.assertIn("bad phase", str(e.exception))
        self.assertIn("needs `apt` or `provider`", str(e.exception))


class EnvTests(unittest.TestCase):
    def test_gpu_detection_skips_connectors(self):
        s = FakeSystem(files={
            "/sys/class/drm/card0": None, "/sys/class/drm/card0/device/vendor": "0x8086\n",
            "/sys/class/drm/card0-eDP-1": None,
            "/sys/class/drm/card1": None, "/sys/class/drm/card1/device/vendor": "0x10de\n",
        }, links={"/sys/class/drm/card0/device/driver": "../../bus/pci/drivers/i915",
                  "/sys/class/drm/card1/device/driver": "../../bus/pci/drivers/nvidia"})
        gpus = cd.detect_gpus(s)
        self.assertEqual([(g.vendor, g.driver) for g in gpus], [("intel", "i915"), ("nvidia", "nvidia")])

    def test_nvidia_major_from_proc(self):
        s = FakeSystem(files={"/proc/driver/nvidia/version":
                              "NVRM version: NVIDIA UNIX x86_64 Kernel Module  570.211.01  Tue ...\n"})
        self.assertEqual(cd.detect_nvidia_major(s), "570")

    def test_session_from_env(self):
        s = FakeSystem()
        self.assertEqual(cd.detect_session(s, {"XDG_SESSION_TYPE": "wayland", "XDG_CURRENT_DESKTOP": "ubuntu:GNOME"}),
                         ("wayland", "GNOME", "env"))
        self.assertEqual(cd.detect_session(s, {"WAYLAND_DISPLAY": "wayland-1", "SWAYSOCK": "/x"})[:2],
                         ("wayland", "sway"))

    def test_session_from_loginctl_over_ssh(self):
        s = FakeSystem(commands={
            r"loginctl list-sessions --no-legend": (0, "  2 1000 jo seat0 tty2\n  7 1000 jo -    pts/1\n"),
            r"loginctl show-session 2 .*": (0, "Type=wayland\nDesktop=KDE\nClass=user\nState=active\n"),
            r"loginctl show-session 7 .*": (0, "Type=tty\nDesktop=\nClass=user\nState=active\n"),
        }, bins=["loginctl"])
        import getpass
        orig = getpass.getuser
        getpass.getuser = lambda: "jo"
        try:
            self.assertEqual(cd.detect_session(s, {"SSH_CONNECTION": "x"}), ("wayland", "KDE", "loginctl"))
        finally:
            getpass.getuser = orig

    def test_detect_env_ubuntu(self):
        s = FakeSystem(files=dict(UBUNTU), commands={r"dpkg-architecture .*": (0, "x86_64-linux-gnu\n")})
        e = cd.detect_env(s, args(session="x11", desktop="XFCE", gpu=["amd"]))
        self.assertEqual((e.distro, e.family, e.multiarch), ("ubuntu", "apt", "x86_64-linux-gnu"))
        self.assertEqual(e.gpu_vendors(), {"amd"})


class ResolutionTests(unittest.TestCase):
    def apt(self, policy, installed=(), names=""):
        cmds = {r"apt-cache pkgnames .*": (0, names)}
        for pkg, ver in policy.items():
            cmds[rf"apt-cache policy {re.escape(pkg)}"] = (0, f"{pkg}:\n  Installed: (none)\n  Candidate: {ver}\n")
        for pkg in installed:
            cmds[rf"dpkg-query -W -f=\$\{{db:Status-Status\}} {re.escape(pkg)}"] = (0, "installed")
        return cd.Apt(FakeSystem(commands=cmds))

    def env(self, **kw):
        e = cd.Env(multiarch="x86_64-linux-gnu", kernel="6.8.0-45-generic")
        for k, v in kw.items():
            setattr(e, k, v)
        return e

    def test_versioned_picks_newest_satisfying(self):
        apt = self.apt({"cargo-1.91": "1.91.1", "rustc-1.91": "1.91.1", "cargo-1.89": "1", "rustc-1.89": "1"},
                       names="cargo-1.80\ncargo-1.89\ncargo-1.91\ncargo-1.91-doc\n")
        dep = {"apt": {"versioned": "cargo-1.{minor}", "also": ["rustc-1.{minor}"], "min": "1.91"}}
        self.assertEqual(cd.resolve_apt(dep, self.env(), apt).packages, ["cargo-1.91", "rustc-1.91"])

    def test_versioned_none_new_enough(self):
        apt = self.apt({"cargo-1.85": "1", "rustc-1.85": "1"}, names="cargo-1.85\n")
        dep = {"apt": {"versioned": "cargo-1.{minor}", "also": ["rustc-1.{minor}"], "min": "1.91"}}
        r = cd.resolve_apt(dep, self.env(), apt)
        self.assertEqual(r.packages, [])
        self.assertEqual(r.unavailable, ["cargo-1.>=91"])

    def test_first_available_candidate_with_note(self):
        apt = self.apt({"intel-media-va-driver-non-free": "(none)", "intel-media-va-driver": "24.1"})
        r = cd.resolve_apt({"apt": ["intel-media-va-driver-non-free", "intel-media-va-driver"]}, self.env(), apt)
        self.assertEqual(r.packages, ["intel-media-va-driver"])
        self.assertIn("non-free not available", r.note)

    def test_installed_but_failing_is_not_reinstalled(self):
        apt = self.apt({"gstreamer1.0-plugins-bad": "1.24"}, installed=["gstreamer1.0-plugins-bad"])
        r = cd.resolve_apt({"apt": "gstreamer1.0-plugins-bad"}, self.env(), apt)
        self.assertEqual((r.packages, r.installed_but_failing), ([], ["gstreamer1.0-plugins-bad"]))

    def test_min_version_rejects_old_candidate(self):
        apt = self.apt({"xdg-desktop-portal-kde": "5.27.11-0ubuntu3"})
        r = cd.resolve_apt({"apt": {"pkg": "xdg-desktop-portal-kde", "min": "6.0"}}, self.env(), apt)
        self.assertEqual(r.packages, [])
        self.assertIn("offer 5.27.11", r.unavailable[0])

    def test_templates(self):
        apt = self.apt({"libnvidia-encode-570": "570.211", "linux-headers-6.8.0-45-generic": "x"})
        self.assertEqual(cd.resolve_apt({"apt": "libnvidia-encode-{nvidia_major}"},
                                        self.env(nvidia_major="570"), apt).packages, ["libnvidia-encode-570"])
        self.assertIn("nvidia_major", cd.resolve_apt({"apt": "libnvidia-encode-{nvidia_major}"},
                                                     self.env(), apt).note)
        self.assertEqual(cd.resolve_apt({"apt": "linux-headers-{kernel}"}, self.env(), apt).packages,
                         ["linux-headers-6.8.0-45-generic"])


class CheckTests(unittest.TestCase):
    def test_applies(self):
        e = cd.Env(session="wayland", desktop="GNOME", gpus=[cd.Gpu("c0", "intel", "i915")])
        self.assertTrue(cd.applies({"when": {"session": ["wayland"], "desktop": ["GNOME"]}}, e)[0])
        self.assertFalse(cd.applies({"when": {"desktop": ["KDE"]}}, e)[0])
        self.assertTrue(cd.applies({"when": {"gpu": ["amd", "intel"]}}, e)[0])
        self.assertFalse(cd.applies({"when": {"gpu_driver": ["nvidia"]}}, e)[0])
        self.assertFalse(cd.applies({"features": ["evdi"]}, e)[0])
        e.features = ["evdi"]
        self.assertTrue(cd.applies({"features": ["evdi"]}, e)[0])

    def test_toolchain_prefers_newest_ok(self):
        s = FakeSystem(bins=["cargo", "rustc", "/usr/lib/rust-1.91/bin/cargo", "/usr/lib/rust-1.91/bin/rustc"],
                       files={"/usr/lib/rust-1.91/bin/cargo": None},
                       commands={r"/usr/bin/cargo --version": (0, "cargo 1.75.0 (ubuntu)"),
                                 r"/usr/lib/rust-1.91/bin/cargo --version": (0, "cargo 1.91.1 (x)")})
        e = cd.Env(home="/nonexistent")
        pr = cd.Prober(s, e).probe({"kind": "toolchain", "min": "1.91"})
        self.assertTrue(pr.ok)
        self.assertEqual(pr.found, "/usr/lib/rust-1.91/bin/cargo")
        s.bins -= {"/usr/lib/rust-1.91/bin/cargo"}
        pr = cd.Prober(s, e).probe({"kind": "toolchain", "min": "1.91"})
        self.assertFalse(pr.ok)
        self.assertIn("1.75.0 (too old)", pr.detail)

    def test_gst_element_falls_back_to_plugin_files(self):
        s = FakeSystem(files={"/usr/lib/x86_64-linux-gnu/gstreamer-1.0/libgstrtp.so": None})
        e = cd.Env(multiarch="x86_64-linux-gnu")
        p = cd.Prober(s, e)
        self.assertTrue(p.probe({"kind": "gst-element", "elements": ["rtph264pay"], "plugin": "rtp"}).ok)
        self.assertFalse(p.probe({"kind": "gst-element", "elements": ["rtpbin"], "plugin": ["rtp", "rtpmanager"]}).ok)

    def test_needs_session_bus_is_unverified_not_missing(self):
        e = cd.Env(user_bus=False)
        pr = cd.Prober(FakeSystem(), e).probe({"kind": "dbus-name", "name": "x", "needs": ["user-bus"]})
        self.assertTrue(pr.unverified)

    def test_full_check_on_fake_gnome_laptop(self):
        """End to end over the real manifest: GNOME + Intel, VA driver missing."""
        deps = cd.load_manifest(MANIFEST)
        s = FakeSystem(bins=["gst-inspect-1.0", "pkg-config", "busctl", "systemctl"],
                       files={"/dev/dri/renderD128": None},
                       commands={r"gst-inspect-1.0 --exists vah264enc": (1, ""),
                                 r"gst-inspect-1.0 --exists .*": (0, ""),
                                 r"busctl .*": (0, "b true"),
                                 r"systemctl --user is-active pipewire.service": (0, "active")})
        e = cd.Env(session="wayland", desktop="GNOME", gpus=[cd.Gpu("card0", "intel", "i915")],
                   multiarch="x86_64-linux-gnu", user_bus=True, family=None)
        by_id = {r.id: r for r in cd.check(deps, e, s, {"runtime"})}
        self.assertEqual(by_id["gst-pipewire"].status, "ok")
        self.assertEqual(by_id["mutter-screencast"].status, "ok")
        self.assertEqual(by_id["va-intel"].status, "missing")
        self.assertEqual(by_id["gst-va"].status, "missing")
        self.assertEqual(by_id["portal-kde"].status, "skipped")
        self.assertEqual(by_id["nvenc-lib"].status, "skipped")
        self.assertEqual(by_id["render-node"].status, "ok")


if __name__ == "__main__":
    unittest.main()
