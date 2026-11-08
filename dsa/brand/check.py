# /// script
# requires-python = ">=3.10"
# dependencies = []
# ///
"""Checks that every icon file DSA ships is there and holds every size it must.

    uv run dsa/brand/check.py           # the files where they ship
    uv run dsa/brand/check.py --out D   # the files a `generate.py --out D` run wrote

Each check prints what is missing and the run fails if anything is.
"""
import json
import struct
import sys
from pathlib import Path

import icons

failures = []


def fail(message):
    failures.append(message)


def located(out, path):
    return out / path.relative_to(icons.ROOT) if out else path


def png_size(data):
    if data[:8] != b"\x89PNG\r\n\x1a\n" or data[12:16] != b"IHDR":
        return None
    return struct.unpack(">II", data[16:24])


def ico_sizes(data):
    if len(data) < 6 or struct.unpack("<HHH", data[:6])[:2] != (0, 1):
        return None
    sizes = []
    for index in range(struct.unpack("<H", data[4:6])[0]):
        entry = 6 + 16 * index
        length, offset = struct.unpack("<II", data[entry + 8:entry + 16])
        size = png_size(data[offset:offset + length])
        sizes.append(size[0] if size and size[0] == size[1] else None)
    return sizes


def icns_chunks(data):
    if data[:4] != b"icns":
        return None
    chunks, at = {}, 8
    while at + 8 <= len(data):
        kind, length = data[at:at + 4], struct.unpack(">I", data[at + 4:at + 8])[0]
        chunks[kind] = png_size(data[at + 8:at + length])
        at += max(length, 8)
    return chunks


def read(out, path):
    path = located(out, path)
    if not path.is_file() or path.stat().st_size == 0:
        fail(f"{path.relative_to(out or icons.ROOT)} is missing")
        return None
    return path.read_bytes()


def fr17_every_windows_icon_holds_every_size(out):
    files = [path.with_suffix(".ico") for path, _ in icons.WINDOWS_ICONS]
    files += list(icons.WINDOWS_TRAY.values())
    for path in files:
        data = read(out, path)
        if data is None:
            continue
        sizes = ico_sizes(data)
        if sizes is None:
            fail(f"{path.name} is not an icon file")
            continue
        missing = [s for s in icons.ICO_SIZES if s not in sizes]
        if missing:
            fail(f"{path.name} lacks the sizes {missing} (it holds {sizes})")
    for path, _ in icons.WINDOWS_ICONS:
        data = read(out, path.with_suffix(".png"))
        if data is not None and png_size(data) != (256, 256):
            fail(f"{path.with_suffix('.png').name} is not a 256 px PNG image")


def fr17_every_linux_app_has_a_colour_and_a_symbolic_icon(out):
    for app_id in icons.LINUX_APPS.values():
        for path in icons.linux_paths(app_id):
            data = read(out, path)
            if data is not None and b"<svg" not in data:
                fail(f"{path.name} is not an SVG image")


def fr17_the_macos_icon_holds_every_size_and_its_layers(out):
    for path in [icons.MACOS_ICNS, *icons.MACOS_APP_ICNS.values()]:
        data = read(out, path)
        if data is None:
            continue
        chunks = icns_chunks(data)
        if chunks is None:
            fail(f"{path.name} is not an .icns file")
            continue
        for kind, points, scale in icons.ICNS_IMAGES:
            pixels = points * scale
            if chunks.get(kind) != (pixels, pixels):
                fail(f"{path.name} lacks the {points} pt @{scale}x image ({kind.decode()})")
    manifest = read(out, icons.MACOS_ICON / "icon.json")
    if manifest is not None:
        names = {
            layer["image-name"]
            for group in json.loads(manifest)["groups"]
            for layer in group["layers"]
        }
        for name in icons.MACOS_LAYERS:
            if f"{name}.png" not in names:
                fail(f"DSA.icon/icon.json does not name the layer {name}.png")
            layer = read(out, icons.MACOS_ICON / "Assets" / f"{name}.png")
            size = icons.MACOS_LAYER_SIZE
            if layer is not None and png_size(layer) != (size, size):
                fail(f"the layer {name}.png is not {size} px")
    for path, pixels in icons.MACOS_MENU_BAR:
        image = read(out, path)
        if image is not None and png_size(image) != (pixels, pixels):
            fail(f"{path.name} is not {pixels} px")


def fr17_the_readme_logo_is_there(out):
    data = read(out, icons.README_LOGO)
    size = icons.README_LOGO_SIZE
    if data is not None and png_size(data) != (size, size):
        fail(f"the README logo is not {size} px")


def fr11_the_apk_installer_is_the_open_box_with_no_badge(out):
    """The APK Installer ships the open box master on every host, with no badge on it and
    not the DSA symbol.

    The colour master is the open cardboard box with plain fills and gradients only (no
    filter or mask, so every renderer draws it the same), and the symbolic one is that box
    as a one colour outline on the 16 px grid. The earlier parcel with an arrow fails both.
    """
    for app, name in icons.OWN_MASTERS.items():
        own = (icons.BRAND / f"{name}.svg").read_text(encoding="utf-8")
        if "open cardboard box" not in own or "arrow" in own:
            fail(f"{name}.svg is not the open cardboard box")
        if "<filter" in own or "<mask" in own:
            fail(f"{name}.svg uses a filter or a mask, which renderers draw differently")
        outline = (icons.BRAND / f"{name}-symbolic.svg").read_text(encoding="utf-8")
        flat = 'viewBox="0 0 16 16"' in outline and 'fill="none"' in outline
        if not flat or "arrow" in outline or "<mask" in outline:
            fail(f"{name}-symbolic.svg is not the box as an outline on the 16 px grid")
        symbol = (icons.BRAND / "symbol.svg").read_text(encoding="utf-8")
        if inner_paths(symbol) & inner_paths(own):
            fail(f"{name}.svg reuses the DSA symbol's shapes")
        scalable, symbolic = icons.linux_paths(icons.LINUX_APPS[app])
        for path, master in ((scalable, name), (symbolic, f"{name}-symbolic")):
            data = read(out, path)
            expected = (icons.BRAND / f"{master}.svg").read_bytes()
            if data is not None and data.replace(b"\r\n", b"\n") != expected.replace(b"\r\n", b"\n"):
                fail(f"{path.name} is not {master}.svg")
        path = next(p for p, a in icons.WINDOWS_ICONS if a == app)
        ico_data, png = read(out, path.with_suffix(".ico")), read(out, path.with_suffix(".png"))
        if ico_data is not None and png is not None and png not in ico_data:
            fail(f"{path.name}.ico and {path.name}.png are not the same drawing")
        icns = read(out, icons.MACOS_APP_ICNS[app])
        # The 256 px image of the .icns is the same rendering as the Windows one.
        if icns is not None and png is not None and png not in icns:
            fail(f"{icons.MACOS_APP_ICNS[app].name} is not {name}.svg")
        has_badge(out, app)


def plain_symbol(out, app):
    """Fails unless every icon of `app` on every host is the plain DSA symbol: the same files
    as DSA's own icon, so nothing is drawn on top of it."""
    plain_windows = icons.WINDOWS / "installer" / "setup-ui" / "assets" / "logo"
    for path, _ in (p for p in icons.WINDOWS_ICONS if p[1] == app):
        for suffix in (".ico", ".png"):
            ours = read(out, path.with_suffix(suffix))
            plain = read(out, plain_windows.with_suffix(suffix))
            if ours is not None and plain is not None and ours != plain:
                fail(f"{path.name}{suffix} ({app}) is not the plain DSA symbol of logo{suffix}")
    plain_linux = icons.linux_paths(icons.LINUX_APPS[None])
    for ours_path, plain_path in zip(icons.linux_paths(icons.LINUX_APPS[app]), plain_linux):
        ours, plain = read(out, ours_path), read(out, plain_path)
        if ours is not None and plain is not None and lf(ours) != lf(plain):
            fail(f"{ours_path.name} is not the plain DSA symbol of {plain_path.name}")
    ours, plain = read(out, icons.MACOS_APP_ICNS[app]), read(out, icons.MACOS_ICNS)
    if ours is not None and plain is not None and ours != plain:
        fail(f"{icons.MACOS_APP_ICNS[app].name} is not the plain DSA symbol of DSA.icns")
    has_badge(out, app)


# What a badge is drawn with: the white disc's shadow and the cut the symbolic icon made
# around it, and the disc itself, in the lower right corner of the 64 unit grid.
BADGE_MARKS = [b"badge", b'cx="51" cy="51"']


def lf(text):
    """A text file with the line ends a checkout on Windows may have given it undone."""
    return text.replace(b"\r\n", b"\n")


def has_badge(out, app):
    """Fails if a vector icon of `app`, or the master it is drawn from, draws a badge."""
    paths = list(icons.linux_paths(icons.LINUX_APPS[app]))
    if app in icons.OWN_MASTERS:
        own = icons.OWN_MASTERS[app]
        paths += [icons.BRAND / f"{own}.svg", icons.BRAND / f"{own}-symbolic.svg"]
    for path in paths:
        data = path.read_bytes() if path.parent == icons.BRAND else read(out, path)
        if data is not None and any(mark in data for mark in BADGE_MARKS):
            fail(f"{path.name} ({app}) draws a badge")


def fr17_the_settings_icon_has_no_badge(out):
    """Settings is the plain DSA symbol on Windows, Linux and macOS, with no gear badge."""
    plain_symbol(out, "settings")


def fr29_the_files_icon_has_no_badge(out):
    """Android's entry in Explorer's navigation pane and DSA Files on Linux and macOS are
    the plain DSA symbol, like the plain penguin of WSL's "Linux", with no folder badge."""
    plain_symbol(out, "files")


def fr17_no_icon_has_a_badge(out):
    """Every companion app is checked, and the generator can no longer draw a badge."""
    checked = {"settings", "files", *icons.OWN_MASTERS}
    if set(icons.COMPANION_APPS) != checked:
        fail(f"the companion apps {sorted(set(icons.COMPANION_APPS) - checked)} are not checked")
    for app in icons.COMPANION_APPS:
        has_badge(out, app)
    generator = (icons.BRAND / "generate.py").read_text(encoding="utf-8")
    for drawing in ("feDropShadow", "badge-", "with_badge", "glyph("):
        if drawing in generator:
            fail(f"generate.py draws a badge ({drawing})")
    for svg in sorted(icons.BRAND.glob("*.svg")):
        if any(mark in svg.read_bytes() for mark in BADGE_MARKS):
            fail(f"the master {svg.name} draws a badge")


def inner_paths(svg):
    import re

    return set(re.findall(r'\sd="([^"]+)"', svg))


def main(argv):
    out = Path(argv[1]).resolve() if argv[:1] == ["--out"] and len(argv) == 2 else None
    checks = [
        fr17_every_windows_icon_holds_every_size,
        fr17_every_linux_app_has_a_colour_and_a_symbolic_icon,
        fr17_the_macos_icon_holds_every_size_and_its_layers,
        fr17_the_readme_logo_is_there,
        fr17_the_settings_icon_has_no_badge,
        fr11_the_apk_installer_is_the_open_box_with_no_badge,
        fr29_the_files_icon_has_no_badge,
        fr17_no_icon_has_a_badge,
    ]
    for check in checks:
        before = len(failures)
        check(out)
        print(("ok  " if len(failures) == before else "FAIL") + "  " + check.__name__)
    for message in failures:
        print("  " + message)
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main(sys.argv[1:])
