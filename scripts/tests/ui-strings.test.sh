#!/usr/bin/env bash
# The words the desktop apps, the setup window and the Windows shell show:
# - no settings string names the shared folders' old place ("Windows" or "Mac" folder in
#   Android's shared storage); the folder there is "Host" on every host;
# - Korean UI never transliterates Android, which is a trademark, and calls the ARM
#   translation package by one name;
# - the Windows installer has the Korean words: the package's ko-KR localization, the setup
#   window's Korean text, and the shell string table that Explorer, the Start menu and the
#   Services console read in the user's language.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run --no-project python - <<'PY'
import glob
import json
import re
import sys
import xml.etree.ElementTree as ET

failed = []

def fail(message):
    failed.append(message)
    print(f"not ok - {message}")

windows = "dsa/host/platform/windows"
settings_catalogs = (
    glob.glob(f"{windows}/settings/DsaSettings/Strings*.resx")
    + glob.glob("dsa/host/platform/linux/settings/po/*.po")
    + glob.glob("dsa/host/platform/linux/settings/src/**/*.rs", recursive=True)
    + glob.glob("dsa/host/platform/macos/settings/Resources/*.xcstrings")
)
old_folder = re.compile(r"in the (Windows|Mac) folder|의 (Windows|Mac) 폴더")
for path in settings_catalogs:
    for number, line in enumerate(open(path, encoding="utf-8"), 1):
        if old_folder.search(line):
            fail(f"fr9_settings_strings_name_the_host_folder: {path}:{number} names the old folder")

# Korean UI resources. The Linux desktop entry's Keywords[ko] keeps the transliteration on
# purpose: it is what a Korean user types to search for the app, never shown as a name.
korean_ui = [
    f"{windows}/settings/DsaSettings/Strings.ko.resx",
    "dsa/host/platform/linux/settings/po/ko.po",
    "dsa/host/platform/linux/settings/data/io.github.thisisthepy.DSA.Settings.metainfo.xml",
    f"{windows}/installer/dsa.ko-kr.wxl",
    f"{windows}/installer/setup-ui/Text.cs",
    f"{windows}/resources/src/lib.rs",
]
korean_text = []
for path in korean_ui:
    korean_text.append((path, open(path, encoding="utf-8").read()))
catalog = json.load(open("dsa/host/platform/macos/settings/Resources/Localizable.xcstrings", encoding="utf-8"))
for key, entry in catalog["strings"].items():
    value = entry.get("localizations", {}).get("ko", {}).get("stringUnit", {}).get("value", "")
    korean_text.append((f"Localizable.xcstrings {key}", value))
for path in glob.glob("dsa/host/platform/linux/**/*.desktop", recursive=True):
    for line in open(path, encoding="utf-8"):
        if re.match(r"(Name|GenericName|Comment)\[ko\]=", line):
            korean_text.append((path, line))
for path, text in korean_text:
    if "안드로이드" in text and not path.endswith("lib.rs"):
        fail(f"korean_ui_writes_android_as_the_trademark: {path} transliterates Android")
    if re.search(r"ARM 변환|ARM 번역 패키지|ARM 변환 패키지", text):
        fail(f"korean_ui_names_the_arm_translation_package_once: {path} uses another name than ARM 번역 묶음")

# The installer's Korean words.
def wxl(path):
    root = ET.parse(path).getroot()
    return {s.get("Id"): s.get("Value") for s in root.iter() if s.tag.endswith("String")}

english, korean = wxl(f"{windows}/installer/dsa.en-us.wxl"), wxl(f"{windows}/installer/dsa.ko-kr.wxl")
if set(english) != set(korean):
    fail(f"fr3_installer_has_korean_strings: dsa.ko-kr.wxl ids {sorted(korean)} differ from {sorted(english)}")
for name, value in korean.items():
    if not re.search(r"[가-힣]", value or ""):
        fail(f"fr3_installer_has_korean_strings: {name} in dsa.ko-kr.wxl is not Korean")
wxs = open(f"{windows}/installer/dsa.wxs", encoding="utf-8").read()
for name in english:
    if f"!(loc.{name})" not in wxs:
        fail(f"fr3_installer_has_korean_strings: dsa.wxs does not use !(loc.{name})")
if "dsa.ko-kr.wxl" not in open(f"{windows}/installer/build.ps1", encoding="utf-8").read():
    fail("fr3_installer_has_korean_strings: build.ps1 does not pass dsa.ko-kr.wxl")

# The shell words: each one dsa.wxs points at is in the string table with Korean.
lib = open(f"{windows}/resources/src/lib.rs", encoding="utf-8").read()
ids = dict(re.findall(r"pub const (\w+): u16 = (\d+);", lib.split("pub mod shell", 1)[1].split("pub const STRINGS", 1)[0]))
string = r'"((?:[^"\\]|\\.)*)"'
table = re.findall(r'\(\s*(\w+),\s*' + string + r',\s*' + string + r',?\s*\)', lib)
korean_by_id = {ids[name]: ko for name, _, ko in table if name in ids}
for id_ in ids.values():
    if id_ not in korean_by_id:
        fail(f"fr3_shell_words_have_korean: string {id_} has no Korean")
# A string is named "@file,-id"; "file,-id" without the @ is an icon (DefaultIcon), not a string.
used = set(re.findall(r"@\[INSTALLFOLDER\]dsa-app-window\.exe,-(\d+)", wxs)) | set(re.findall(r'(?:Description|Display)ResourceId="(\d+)"', wxs))
for id_ in sorted(used):
    if id_ not in korean_by_id:
        fail(f"fr3_shell_words_have_korean: dsa.wxs points at string {id_}, which the table lacks")
for verb in ["Open in an Android app", "Share with an Android app", "Install in Android"]:
    if f'Value="{verb}"' in wxs:
        fail(f"fr3_shell_words_have_korean: dsa.wxs writes \"{verb}\" in English only")
for needed in ["Android 앱에서 열기", "Android 앱으로 공유"]:
    if needed not in lib:
        fail(f"fr3_shell_words_have_korean: the shell string table has no \"{needed}\"")

# The setup window has Korean for every English line it shows.
text_cs = open(f"{windows}/installer/setup-ui/Text.cs", encoding="utf-8").read()
for name, english_text, ko in re.findall(r'public static string (\w+) => Pick\(\s*' + string + r'(?:\s*\+\s*"(?:[^"\\]|\\.)*")*,\s*' + string, text_cs):
    if name != "FailureLogBefore" and not re.search(r"[가-힣]", ko):
        fail(f"fr3_setup_window_has_korean: {name} has no Korean")
if "CurrentUICulture" not in text_cs:
    fail("fr3_setup_window_has_korean: the setup window does not follow the display language")

if failed:
    sys.exit(1)
print(f"ok   - {len(settings_catalogs)} settings catalogs, {len(korean_text)} Korean texts, {len(used)} shell strings")
PY
