"""Checks a guest image made by make-image.py.

    uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615

It fails when the shell apps are still there, when the build is still debuggable, when
anything is still signed with AOSP's test keys, or when the files do not match image.json.
"""

import hashlib
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

image = Path(sys.argv[1])
manifest = json.loads((image / "image.json").read_text())
problems = []

for name, expected in manifest["files"].items():
    h = hashlib.sha256()
    with open(image / name, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    if h.hexdigest() != expected["sha256"]:
        problems.append(f"{name} does not match image.json")

with zipfile.ZipFile(image / "signed-target_files.zip") as z:
    names = z.namelist()
    for app in ["Launcher3QuickStep", "SystemUI", "LatinIME"]:
        if any(f"/{app}/" in n and n.endswith(".apk") for n in names):
            problems.append(f"{app} is still in the image")
    props = z.read("SYSTEM/build.prop").decode()
    if "ro.debuggable=1" in props or "ro.build.type=userdebug" in props:
        problems.append("the system is still a debuggable build")
    if "test-keys" in props:
        problems.append("the build still says test-keys")
    certs = z.read("META/apkcerts.txt").decode()
    test = [l for l in certs.splitlines() if re.search(r'certificate="build/make/target/product/security/(testkey|platform|shared|media)', l)]
    if test:
        problems.append(f"{len(test)} packages are still signed with AOSP's test keys, e.g. {test[0][:120]}")
    if "VENDOR/etc/fstab.dsa" not in names:
        problems.append("the Hyper-V mount table is missing")

info = json.loads(subprocess.run(["qemu-img", "info", "--output=json", str(image / "disk.vhdx")], check=True, capture_output=True, text=True).stdout)
if info.get("format") != "vhdx":
    problems.append(f"disk.vhdx is {info.get('format')}, not vhdx")
print(f"disk.vhdx: {info['virtual-size'] >> 30} GiB virtual, {info.get('actual-size', 0) >> 20} MiB on disk")
if "androidboot.boot_part_uuid=" not in manifest["kernel_cmdline"]:
    problems.append("the kernel command line does not name the disk")

for p in problems:
    print("problem:", p)
print("image ok" if not problems else f"{len(problems)} problems")
sys.exit(1 if problems else 0)
