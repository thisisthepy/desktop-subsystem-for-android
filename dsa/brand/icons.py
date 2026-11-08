"""Every icon file DSA ships, where it goes, and the sizes it holds.

Both the generator (`generate.py`) and the size check (`check.py`) read this list, so a
file the generator writes is a file the check looks for.
"""
from pathlib import Path

BRAND = Path(__file__).resolve().parent
DSA = BRAND.parent
ROOT = DSA.parent

# The sizes a Windows .ico holds. 24 px and below are drawn from the small masters.
ICO_SIZES = [16, 20, 24, 32, 40, 48, 64, 256]
SMALL_MAX = 24

# The images of a macOS .icns: (chunk type, point size, scale).
ICNS_IMAGES = [
    (b"icp4", 16, 1), (b"ic11", 16, 2),
    (b"icp5", 32, 1), (b"ic12", 32, 2),
    (b"icp6", 64, 1),
    (b"ic07", 128, 1), (b"ic13", 128, 2),
    (b"ic08", 256, 1), (b"ic14", 256, 2),
    (b"ic09", 512, 1), (b"ic10", 512, 2),
]

# The companion apps. No icon carries a badge: Settings and Files are the plain DSA symbol,
# the same drawing as DSA's own icon, because a mark in the corner is too small to read at
# 16 px and makes three icons of one program.
COMPANION_APPS = ["settings", "apk", "files"]

# Companion apps drawn from their own master instead of the plain symbol: the APK Installer
# is an open cardboard box, with no DSA mark on it. Each also has a `<master>-symbolic.svg`
# for Linux.
OWN_MASTERS = {"apk": "apk-installer"}

WINDOWS = DSA / "host" / "platform" / "windows"
LINUX_ICONS = DSA / "host" / "platform" / "linux" / "settings" / "data" / "icons" / "hicolor"
MACOS = DSA / "host" / "platform" / "macos" / "packaging" / "icon"
LINUX_ID = "io.github.thisisthepy.DSA"

# Windows: (file, companion app or None for DSA itself). Each .ico holds ICO_SIZES; the PNG
# beside it is 256 px.
WINDOWS_ICONS = [
    (WINDOWS / "installer" / "setup-ui" / "assets" / "logo", None),
    (WINDOWS / "settings" / "DsaSettings" / "Assets" / "logo", "settings"),
    (WINDOWS / "settings" / "DsaApkInstaller" / "Assets" / "apk-installer", "apk"),
    # Android's entry in Explorer's navigation pane, the plain symbol, like WSL's "Linux"
    # beside it is the plain penguin.
    (WINDOWS / "resources" / "icons" / "files", "files"),
]
# The notification area: one colour, for a light and for a dark taskbar.
WINDOWS_TRAY = {
    "light": WINDOWS / "resources" / "icons" / "tray-light.ico",
    "dark": WINDOWS / "resources" / "icons" / "tray-dark.ico",
}

# Linux: the colour and the symbolic SVG of DSA and of each companion app.
LINUX_APPS = {
    None: LINUX_ID,
    "settings": LINUX_ID + ".Settings",
    "apk": LINUX_ID + ".ApkInstaller",
    "files": LINUX_ID + ".Files",
}


def linux_paths(app_id):
    return (
        LINUX_ICONS / "scalable" / "apps" / f"{app_id}.svg",
        LINUX_ICONS / "symbolic" / "apps" / f"{app_id}-symbolic.svg",
    )


MACOS_ICNS = MACOS / "DSA.icns"
# Each companion app's own .icns, with the same images as DSA.icns. The settings app is
# DSA.app itself, so Settings.icns and Files.icns are the same file as DSA.icns.
MACOS_APP_ICNS = {
    "settings": MACOS / "Settings.icns",
    "apk": MACOS / "ApkInstaller.icns",
    "files": MACOS / "Files.icns",
}
MACOS_ICON = MACOS / "DSA.icon"
MACOS_LAYERS = ["title-bar", "window"]
MACOS_LAYER_SIZE = 1024
# The menu bar template image, 18 points: (file, pixels).
MACOS_MENU_BAR = [(MACOS / "MenuBarTemplate.png", 18), (MACOS / "MenuBarTemplate@2x.png", 36)]

# The README's logo.
README_LOGO = ROOT / ".github" / "assets" / "dsa-logo.png"
README_LOGO_SIZE = 256
