# /// script
# requires-python = ">=3.11"
# ///
"""Checks the trademark rules on every user-facing name and notice.

Names: the product, binaries, icons, window titles, the service, shortcuts and the
installer's folders and files. None may use a Microsoft mark ("Windows", "Microsoft" and
the others below), and none may start with "Android": Android may appear only after
"for", as in "Desktop Subsystem for Android".

Notices: the README files, the setup program, the MSI, the settings app's About page and
the APK install window carry "Android is a trademark of Google LLC.", and the README
files and the setup program say that DSA is independent of Microsoft and Google. The
first mention of Android in a README reads "Android™".

Usage: nfr6_trademarks.py [repo root]. Prints each problem and exits 1 if there is one.
"""

import re
import sys
import tomllib
from pathlib import Path

MICROSOFT_MARKS = ["Windows", "Microsoft", "Win32", "Xbox", "Azure", "Hyper-V", "WSL", "DirectX", "Bing"]

TRADEMARK = ["Android is a trademark of Google LLC.", "Android는 Google LLC의 상표"]
INDEPENDENT = ["not affiliated with", "제휴하거나"]

SKIP_DIRS = {"target", "build", "out", "bin", "obj", ".gradle", "node_modules", ".scratch", ".claude", ".git"}

WINDOWS = "dsa/host/platform/windows"
SETTINGS = f"{WINDOWS}/settings/DsaSettings"
SETUP_UI = f"{WINDOWS}/installer/setup-ui"

# Each surface that must carry notices, and which ones.
NOTICES = {
    "README.md": (TRADEMARK, INDEPENDENT),
    "docs/locales/README_ko.md": (TRADEMARK, INDEPENDENT),
    f"{SETUP_UI}/Text.cs": (TRADEMARK, INDEPENDENT),
    f"{WINDOWS}/installer/dsa.wxs": (TRADEMARK, INDEPENDENT),
    f"{SETTINGS}/Strings.resx": (TRADEMARK, INDEPENDENT),
    f"{SETTINGS}/Strings.ko.resx": (TRADEMARK, INDEPENDENT),
}

# Each surface that must show a notice string by its resource key.
NOTICE_KEYS = {
    f"{SETTINGS}/Pages/AboutPage.xaml": ["AboutTrademark.Text", "AboutIndependent.Text"],
    f"{SETTINGS}/ApkInstallWindow.xaml.cs": ["AboutTrademark.Text"],
}

# Resource keys whose value is a name: window titles and the product's own name.
NAME_KEYS = re.compile(r"^(WindowTitle|AppTitle\.Text|AboutProductCard\.Header|.*_Caption|.*WindowTitle)$")

# Installer elements and the attributes that name something a user sees.
WXS_NAMES = re.compile(
    r"<(Package|Bundle|ServiceInstall|Shortcut|Directory|File|ProgId)\b([^>]*)>", re.DOTALL)
WXS_ATTRS = re.compile(r'\b(Name|DisplayName)="([^"]*)"')


def name_problems(name: str, title: bool = False) -> list:
    """Why a user-facing name breaks the trademark rules, or nothing."""
    problems = []
    for mark in MICROSOFT_MARKS:
        # A mark inside a longer word ("Windowsill") is not the mark; one joined to DSA's
        # own words by case ("DsaWindows") or a separator ("dsa-windows") is.
        if re.search(rf"(?<![a-z]){re.escape(mark)}(?![a-z])", name, re.IGNORECASE) or re.search(rf"{re.escape(mark)}(?![a-z])", name):
            problems.append(f"uses the Microsoft mark '{mark}'")
            break
    # A window title such as "Install an Android app" says what the window does; it is not a
    # name, so only the Microsoft marks apply to it.
    if not title and re.match(r"\s*android(?![a-z])", name, re.IGNORECASE):
        problems.append("puts 'Android' first; use 'for Android' as in 'Desktop Subsystem for Android'")
    return problems


def is_title(where: str) -> bool:
    return bool(re.search(r"(?i)title|caption", where))


def _files(root: Path, pattern: str):
    for path in sorted(root.rglob(pattern)):
        rel = path.relative_to(root)
        if any(part in SKIP_DIRS for part in rel.parts):
            continue
        yield path


def collect_names(root: Path) -> list:
    """Every (where, name) the trademark rules apply to."""
    names = []
    dsa = root / "dsa"
    for wxs in _files(dsa, "*.wxs"):
        text = wxs.read_text(encoding="utf-8")
        for m in WXS_NAMES.finditer(text):
            for attr, value in WXS_ATTRS.findall(m.group(2)):
                if value and "$(" not in value and "[" not in value:
                    names.append((f"{wxs.relative_to(root).as_posix()}: {m.group(1)} {attr}", value))
    for ps1 in _files(dsa, "build.ps1"):
        for m in re.finditer(r'\$productName\s*=\s*if[^{]*\{\s*"([^"]+)"\s*\}\s*else\s*\{\s*"([^"]+)"', ps1.read_text(encoding="utf-8")):
            names += [(f"{ps1.relative_to(root).as_posix()}: product name", v) for v in m.groups()]
    for csproj in _files(dsa, "*.csproj"):
        text = csproj.read_text(encoding="utf-8")
        where = csproj.relative_to(root).as_posix()
        for tag in ("AssemblyName", "Product", "Title", "AssemblyTitle"):
            for value in re.findall(rf"<{tag}>([^<]+)</{tag}>", text):
                names.append((f"{where}: {tag}", value))
        for value in re.findall(r"<ApplicationIcon>([^<]+)</ApplicationIcon>", text):
            names.append((f"{where}: ApplicationIcon", Path(value.replace("\\", "/")).stem))
    for cargo in _files(dsa, "Cargo.toml"):
        with cargo.open("rb") as f:
            data = tomllib.load(f)
        # Binaries, not crates: a library crate's name never reaches a user.
        where = cargo.relative_to(root).as_posix()
        if "name" in data.get("package", {}) and (cargo.parent / "src/main.rs").exists():
            names.append((f"{where}: binary", data["package"]["name"]))
        for b in data.get("bin", []):
            if "name" in b:
                names.append((f"{where}: binary", b["name"]))
        for b in sorted((cargo.parent / "src/bin").glob("*.rs")):
            names.append((f"{where}: binary", b.stem))
    for resx in _files(dsa, "*.resx"):
        text = resx.read_text(encoding="utf-8")
        for key, value in re.findall(r'<data name="([^"]+)"[^>]*>\s*<value>([^<]*)</value>', text):
            if NAME_KEYS.match(key):
                names.append((f"{resx.relative_to(root).as_posix()}: {key}", value))
    for text_cs in _files(dsa, "Text.cs"):
        text = text_cs.read_text(encoding="utf-8")
        for key, value in re.findall(r'const string (Caption|Title)\s*=\s*"([^"]*)"', text):
            names.append((f"{text_cs.relative_to(root).as_posix()}: {key}", value.replace("\\u2122", "")))
    for xaml in _files(dsa, "*.xaml"):
        text = xaml.read_text(encoding="utf-8")
        for value in re.findall(r'<Window\b[^>]*\bTitle="([^"{]+)"', text, re.DOTALL):
            names.append((f"{xaml.relative_to(root).as_posix()}: window title", value))
    for ext in ("*.ico", "*.png", "*.svg", "*.exe", "*.dll"):
        for icon in _files(dsa, ext):
            names.append((f"{icon.relative_to(root).as_posix()}: file name", icon.stem))
    return names


def notice_problems(root: Path) -> list:
    problems = []
    for rel, groups in NOTICES.items():
        path = root / rel
        if not path.exists():
            problems.append(f"{rel}: missing; it must carry the trademark notice")
            continue
        text = path.read_text(encoding="utf-8")
        for group in groups:
            if not any(phrase in text for phrase in group):
                problems.append(f"{rel}: does not say '{group[0]}'")
    for rel, keys in NOTICE_KEYS.items():
        path = root / rel
        text = path.read_text(encoding="utf-8") if path.exists() else ""
        for key in keys:
            if key not in text:
                problems.append(f"{rel}: does not show the notice {key}")
    for rel in ("README.md", "docs/locales/README_ko.md"):
        path = root / rel
        if path.exists():
            m = re.search(r"Android(?![a-z])", path.read_text(encoding="utf-8"))
            if m and not path.read_text(encoding="utf-8")[m.end():].startswith("™"):
                problems.append(f"{rel}: the first mention of Android must read 'Android™'")
    return problems


def check(root: Path) -> list:
    problems = [f"{where}: '{name}' {why}" for where, name in collect_names(root)
                for why in name_problems(name, title=is_title(where))]
    return problems + notice_problems(root)


def main() -> int:
    root = Path(sys.argv[1]) if len(sys.argv) > 1 else Path(__file__).resolve().parents[2]
    problems = check(root)
    for p in problems:
        print(f"error: {p}", file=sys.stderr)
    if problems:
        return 1
    print("ok    names and notices follow the trademark rules")
    return 0


if __name__ == "__main__":
    sys.exit(main())
