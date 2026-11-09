# /// script
# requires-python = ">=3.10"
# dependencies = ["resvg-py==0.5.0"]
# ///
"""Renders DSA's icons from the master SVGs in this folder.

    uv run dsa/brand/generate.py           # writes every icon where it ships
    uv run dsa/brand/generate.py --out D   # writes them under D instead, same layout

It writes the Windows .ico files (16 to 256 px, the small sizes from the small masters),
the notification area icons, the Linux colour and symbolic SVGs, the macOS .icns (16 to
1024 px) of DSA and of each companion app, its layered Icon Composer source and menu bar
template, and the README logo. No icon carries a badge: Settings and Files are the plain
symbol and the APK Installer is its own open box drawing.
"""
import json
import re
import struct
import sys
from pathlib import Path

import icons

BRAND = icons.BRAND


def master(name):
    return (BRAND / f"{name}.svg").read_text(encoding="utf-8")


def drawings(app, large, small):
    """The large and small drawings of `app` (None for DSA itself): a companion app's own
    master, or the plain symbol. No icon is drawn with a badge."""
    if app in icons.OWN_MASTERS:
        own = master(icons.OWN_MASTERS[app])
        return own, own
    return large, small


def render(svg, size):
    import resvg_py

    png = bytes(resvg_py.svg_to_bytes(svg_string=svg, width=size, height=size))
    if png_size(png) != (size, size):
        raise SystemExit(f"rendering at {size} px gave {png_size(png)}")
    return png


def png_size(png):
    if png[:8] != b"\x89PNG\r\n\x1a\n" or png[12:16] != b"IHDR":
        raise ValueError("not a PNG image")
    return struct.unpack(">II", png[16:24])


def ico(images):
    """An .ico file of PNG entries, one per (size, png)."""
    offset = 6 + 16 * len(images)
    head = struct.pack("<HHH", 0, 1, len(images))
    entries = b""
    for size, data in images:
        entries += struct.pack("<BBBBHHII", size % 256, size % 256, 0, 0, 1, 32, len(data), offset)
        offset += len(data)
    return head + entries + b"".join(data for _, data in images)


def icns(chunks):
    body = b"".join(kind + struct.pack(">I", 8 + len(data)) + data for kind, data in chunks)
    return b"icns" + struct.pack(">I", 8 + len(body)) + body


def write(out, path, data):
    target = out / path.relative_to(icons.ROOT) if out else path
    target.parent.mkdir(parents=True, exist_ok=True)
    if isinstance(data, str):
        target.write_text(data, encoding="utf-8", newline="\n")
    else:
        target.write_bytes(data)


def windows(out):
    large, small = master("symbol"), master("symbol-small")
    for path, app in icons.WINDOWS_ICONS:
        big, little = drawings(app, large, small)
        images = [
            (size, render(little if size <= icons.SMALL_MAX else big, size))
            for size in icons.ICO_SIZES
        ]
        write(out, path.with_suffix(".ico"), ico(images))
        write(out, path.with_suffix(".png"), images[-1][1])
    for theme, path in icons.WINDOWS_TRAY.items():
        big, little = master(f"tray-{theme}"), master(f"tray-{theme}-small")
        images = [
            (size, render(little if size <= icons.SMALL_MAX else big, size))
            for size in icons.ICO_SIZES
        ]
        write(out, path, ico(images))


def linux(out):
    colour, symbolic = master("linux"), master("linux-symbolic")
    for app, app_id in icons.LINUX_APPS.items():
        scalable, symbolic_path = icons.linux_paths(app_id)
        if app in icons.OWN_MASTERS:
            own = icons.OWN_MASTERS[app]
            write(out, scalable, master(own))
            write(out, symbolic_path, master(f"{own}-symbolic"))
        else:
            write(out, scalable, colour)
            write(out, symbolic_path, symbolic)


def macos_layer(svg, keep):
    """One group of the macOS icon on a clear canvas, without the tile and the shadow the
    system draws itself."""
    svg = re.sub(r'<g id="tile">.*?</g>\s*<path[^>]*/>\s*</g>', "", svg, flags=re.S)
    for name in icons.MACOS_LAYERS:
        if name != keep:
            svg = re.sub(rf'<g id="{name}">.*?</g>', "", svg, flags=re.S)
    return svg.replace(' filter="url(#mac-window-shadow)"', "")


def macos(out):
    svg = master("macos")
    chunks = [(kind, render(svg, points * scale)) for kind, points, scale in icons.ICNS_IMAGES]
    write(out, icons.MACOS_ICNS, icns(chunks))
    for app, path in icons.MACOS_APP_ICNS.items():
        drawing, _ = drawings(app, svg, svg)
        if drawing is svg:
            write(out, path, icns(chunks))
        else:
            write(out, path, icns([
                (kind, render(drawing, points * scale))
                for kind, points, scale in icons.ICNS_IMAGES
            ]))
    for name in icons.MACOS_LAYERS:
        layer = macos_layer(svg, name)
        write(out, icons.MACOS_ICON / "Assets" / f"{name}.png", render(layer, icons.MACOS_LAYER_SIZE))
    # The fill is the tile's gradient; the system draws the squircle, its edge and shadow.
    groups = [
        {
            "name": name.replace("-", " ").capitalize(),
            "layers": [{"name": name, "image-name": f"{name}.png", "glass": True}],
            "shadow": {"kind": "neutral", "opacity": 0.25},
            "translucency": {"enabled": True, "value": 0.3 if name == "title-bar" else 0.15},
        }
        for name in icons.MACOS_LAYERS
    ]
    manifest = {
        "fill": {"linear-gradient": ["srgb:0.07843,0.40392,0.29412,1.00000",
                                     "srgb:0.03922,0.24314,0.18039,1.00000"]},
        "groups": groups,
        "supported-platforms": {"squares": ["macOS"]},
    }
    write(out, icons.MACOS_ICON / "icon.json", json.dumps(manifest, indent=2) + "\n")
    template = master("menubar-template")
    for path, pixels in icons.MACOS_MENU_BAR:
        write(out, path, render(template, pixels))


def readme(out):
    write(out, icons.README_LOGO, render(master("symbol"), icons.README_LOGO_SIZE))


def main(argv):
    out = None
    if argv[:1] == ["--out"] and len(argv) == 2:
        out = Path(argv[1]).resolve()
    elif argv:
        raise SystemExit("usage: generate.py [--out DIR]")
    windows(out)
    linux(out)
    macos(out)
    readme(out)


if __name__ == "__main__":
    main(sys.argv[1:])
