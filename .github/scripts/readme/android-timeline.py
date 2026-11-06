# /// script
# requires-python = ">=3.11"
# ///
"""Draws the Android support timeline for the README from android-versions.toml.

Usage: uv run .github/scripts/readme/android-timeline.py [--today YYYY-MM-DD]

Each Android major gets a row: a thin line from its release, and a solid band while DSA
supports it, which ends when the second newer major ships. The picture follows GitHub's
light or dark theme through prefers-color-scheme inside the SVG.
"""

import argparse
import datetime as dt
import tomllib
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
DATA = HERE / "android-versions.toml"
OUT = ROOT / ".github" / "assets" / "android-support.svg"

WIDTH = 860
LEFT = 120
RIGHT = 24
TOP = 46
ROW = 38
BAR = 18


def load(today_override):
    with DATA.open("rb") as f:
        data = tomllib.load(f)
    today = today_override or data["today"]
    rows = sorted(data["android"], key=lambda r: r["major"])
    by_major = {r["major"]: r for r in rows}
    for r in rows:
        later = by_major.get(r["major"] + 2)
        r["dsa_end"] = later["released"] if later else None
        r["end_expected"] = bool(later and later.get("expected"))
    return today, rows


def svg(today, rows):
    start = dt.date(min(r["released"] for r in rows).year, 1, 1)
    last = max([r["dsa_end"] or r["released"] for r in rows] + [today])
    end = dt.date(last.year + 1, 1, 1)
    span = (end - start).days
    plot = WIDTH - LEFT - RIGHT

    def x(day):
        return LEFT + plot * (day - start).days / span

    height = TOP + ROW * len(rows) + 40
    out = [
        f'<svg xmlns="http://www.w3.org/2000/svg" width="{WIDTH}" height="{height}" '
        f'viewBox="0 0 {WIDTH} {height}" role="img" aria-labelledby="t">',
        "<title id=\"t\">Android versions DSA supports, by date</title>",
        "<style>",
        ":root{--fg:#0b2e40;--muted:#5b7380;--grid:#d5e3e8;--band:#2fbf71;"
        "--band-x:#a8e8c4;--line:#1a5873;--today:#e5484d;--bg:#ffffff}",
        "@media (prefers-color-scheme: dark){:root{--fg:#e6f4ee;--muted:#8fa7b2;"
        "--grid:#24404d;--band:#45d483;--band-x:#1f5a3d;--line:#7fb3c8;"
        "--today:#ff6b6e;--bg:#0d1117}}",
        "text{font-family:'Segoe UI',system-ui,-apple-system,sans-serif;fill:var(--fg)}",
        ".m{fill:var(--muted);font-size:12px}.v{font-size:14px;font-weight:600}",
        "</style>",
        f'<rect width="{WIDTH}" height="{height}" rx="12" fill="var(--bg)"/>',
    ]
    for year in range(start.year, end.year + 1):
        gx = x(dt.date(year, 1, 1))
        out.append(f'<line x1="{gx:.1f}" y1="{TOP - 14}" x2="{gx:.1f}" '
                   f'y2="{TOP + ROW * len(rows)}" stroke="var(--grid)"/>')
        if year < end.year:
            mid = x(dt.date(year, 7, 1))
            out.append(f'<text x="{mid:.1f}" y="{TOP - 20}" text-anchor="middle" '
                       f'class="m">{year}</text>')
    for i, r in enumerate(rows):
        cy = TOP + ROW * i + ROW / 2
        rx = x(r["released"])
        label = f"Android {r['major']}"
        out.append(f'<text x="16" y="{cy + 5:.1f}" class="v">{label}</text>')
        dash = ' stroke-dasharray="4 4"' if r.get("expected") else ""
        out.append(f'<line x1="{rx:.1f}" y1="{cy:.1f}" x2="{x(end):.1f}" y2="{cy:.1f}" '
                   f'stroke="var(--grid)" stroke-width="2"{dash}/>')
        out.append(f'<circle cx="{rx:.1f}" cy="{cy:.1f}" r="5" fill="var(--bg)" '
                   f'stroke="var(--line)" stroke-width="2"{dash}/>')
        if "dsa_start" in r:
            sx = x(r["dsa_start"])
            ex = x(r["dsa_end"] or end)
            out.append(f'<rect x="{sx:.1f}" y="{cy - BAR / 2:.1f}" width="{ex - sx:.1f}" '
                       f'height="{BAR}" rx="{BAR / 2}" fill="var(--band)"/>')
            if r["end_expected"] or r["dsa_end"] is None:
                fade = min(60.0, ex - sx)
                out.append(f'<rect x="{ex - fade:.1f}" y="{cy - BAR / 2:.1f}" '
                           f'width="{fade:.1f}" height="{BAR}" rx="{BAR / 2}" '
                           f'fill="var(--band-x)"/>')
            note = "until Android " + str(r["major"] + 2)
            out.append(f'<text x="{sx + 10:.1f}" y="{cy + 4:.1f}" font-size="11" '
                       f'fill="#08261a" style="fill:#08261a">supported {note}</text>')
        else:
            when = "expected" if r.get("expected") else "released"
            out.append(f'<text x="{rx + 10:.1f}" y="{cy - 8:.1f}" class="m">{when} '
                       f'{r["released"]:%b %Y}</text>')
    tx = x(today)
    bottom = TOP + ROW * len(rows)
    out.append(f'<line x1="{tx:.1f}" y1="{TOP - 14}" x2="{tx:.1f}" y2="{bottom + 6}" '
               f'stroke="var(--today)" stroke-width="2"/>')
    out.append(f'<text x="{tx:.1f}" y="{bottom + 20}" text-anchor="middle" class="m" '
               f'style="fill:var(--today)">today {today:%Y-%m-%d}</text>')
    ly = height - 12
    out += [
        f'<circle cx="{LEFT + 6}" cy="{ly - 4}" r="5" fill="none" stroke="var(--line)" stroke-width="2"/>',
        f'<text x="{LEFT + 16}" y="{ly}" class="m">Android release</text>',
        f'<rect x="{LEFT + 130}" y="{ly - 10}" width="28" height="12" rx="6" fill="var(--band)"/>',
        f'<text x="{LEFT + 164}" y="{ly}" class="m">supported by DSA</text>',
        f'<line x1="{LEFT + 290}" y1="{ly - 4}" x2="{LEFT + 318}" y2="{ly - 4}" '
        f'stroke="var(--grid)" stroke-width="2" stroke-dasharray="4 4"/>',
        f'<text x="{LEFT + 324}" y="{ly}" class="m">expected date</text>',
        "</svg>",
    ]
    return "\n".join(out) + "\n"


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--today", type=dt.date.fromisoformat)
    args = parser.parse_args()
    today, rows = load(args.today)
    OUT.parent.mkdir(parents=True, exist_ok=True)
    OUT.write_text(svg(today, rows), encoding="utf-8", newline="\n")
    print(f"wrote {OUT.relative_to(ROOT).as_posix()}")


if __name__ == "__main__":
    main()
