#!/usr/bin/env bash
# The package opens .apk files in the APK Installer, its own program, not in the settings app:
# the .apk association, its Open with entry and its friendly name all point at
# settings\DsaApkInstaller.exe, the package installs that program, and setup closes it before
# replacing it.
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$repo_root"

uv run --no-project python - dsa/host/platform/windows/installer/dsa.wxs <<'PY'
import re
import sys
import xml.etree.ElementTree as ET

NS = {"w": "http://wixtoolset.org/schemas/v4/wxs", "util": "http://wixtoolset.org/schemas/v4/wxs/util"}
EXE = "[SETTINGSFOLDER]DsaApkInstaller.exe"
# The program's name, an indirect string, so each user sees it in their own language.
NAME = "@[INSTALLFOLDER]dsa-app-window.exe,-110"
failed = []


def check(ok, why):
    if not ok:
        failed.append(why)


root = ET.parse(sys.argv[1]).getroot()
values = {}
for value in root.iter(f"{{{NS['w']}}}RegistryValue"):
    key = (value.get("Root"), value.get("Key"), value.get("Name") or "")
    values[key] = value.get("Value")


def get(key, name=""):
    return values.get(("HKLM", key, name))


command = get(r"SOFTWARE\Classes\Dsa.ApkFile\shell\open\command")
check(command == f'"{EXE}" --install-apk "%1"', f"fr11: the .apk association runs {command!r}, not the APK Installer")
check(get(r"SOFTWARE\Classes\.apk") == "Dsa.ApkFile", "fr11: .apk is not associated with Dsa.ApkFile")
check(get(r"SOFTWARE\Classes\.apk\OpenWithProgids", "Dsa.ApkFile") is not None,
      "fr11: the APK Installer is not under Open with for .apk files")
check(get(r"SOFTWARE\Classes\Dsa.ApkFile\DefaultIcon") == f'"{EXE}",0',
      "fr11: .apk files do not show the APK Installer's icon")
check(get(r"SOFTWARE\Classes\Dsa.ApkFile\shell\open", "FriendlyAppName") == NAME,
      "fr11: Open with does not name the APK Installer")
app = r"SOFTWARE\Classes\Applications\DsaApkInstaller.exe"
check(get(app, "FriendlyAppName") == NAME, "fr11: the program has no friendly name for Open with")
check(get(app + r"\SupportedTypes", ".apk") is not None, "fr11: the program does not list .apk as a type it opens")
check(get(app + r"\shell\open\command") == f'"{EXE}" --install-apk "%1"',
      "fr11: the program's own open command does not install the APK")
# Split-APK bundles open in the APK Installer too; App Bundles only under Open with, where it
# says why it does not install them.
BUNDLE_NAME = "@[INSTALLFOLDER]dsa-app-window.exe,-111"
check(get(r"SOFTWARE\Classes\Dsa.ApkBundle\shell\open\command") == f'"{EXE}" --install-apk "%1"',
      "fr11: split-APK bundles do not open in the APK Installer")
check(get(r"SOFTWARE\Classes\Dsa.ApkBundle", "FriendlyTypeName") == BUNDLE_NAME,
      "fr11: split-APK bundles have no type name of their own")
check(get(r"SOFTWARE\Classes\Dsa.ApkBundle\DefaultIcon") == f'"{EXE}",0',
      "fr11: split-APK bundles do not show the APK Installer's icon")
for extension in [".xapk", ".apks", ".apkm"]:
    check(get(rf"SOFTWARE\Classes\{extension}") == "Dsa.ApkBundle",
          f"fr11: {extension} is not associated with Dsa.ApkBundle")
    check(get(rf"SOFTWARE\Classes\{extension}\OpenWithProgids", "Dsa.ApkBundle") is not None,
          f"fr11: the APK Installer is not under Open with for {extension} files")
check(get(r"SOFTWARE\Classes\.aab") is None, "fr11: .aab files are taken from the tools that build them")
check(get(r"SOFTWARE\Classes\.aab\OpenWithProgids", "Dsa.ApkBundle") is not None,
      "fr11: the APK Installer is not under Open with for .aab files")
for extension in [".xapk", ".apks", ".apkm", ".aab"]:
    check(get(app + r"\SupportedTypes", extension) is not None,
          f"fr11: the program does not list {extension} as a type it opens")
aumid = r"SOFTWARE\Classes\AppUserModelId\Thisisthepy.DSA.ApkInstaller"
check(get(aumid, "DisplayName") == NAME, "fr11: the APK Installer's AppUserModelID has no display name")
for value in values.values():
    check(not (value and "DsaSettings.exe" in value and "install-apk" in value),
          f"fr11: something still opens APKs in the settings app: {value}")

files = [f.get("Source", "") for f in root.iter(f"{{{NS['w']}}}File")]
check(any(s.endswith(r"\DsaApkInstaller.exe") for s in files), "fr11: the package does not install DsaApkInstaller.exe")
closes = [c.get("Target") for c in root.iter(f"{{{NS['util']}}}CloseApplication")]
check("DsaApkInstaller.exe" in closes, "fr11: setup does not close the APK Installer before replacing it")
shortcuts = [s.get("Name") for s in root.iter(f"{{{NS['w']}}}Shortcut")]
check("APK Installer" not in shortcuts, "fr11: the APK Installer has a Start menu shortcut, which WSA never had")

for why in failed:
    print(f"not ok - {why}")
if failed:
    sys.exit(1)
print("ok   - .apk files and split-APK bundles open in the APK Installer")
PY
