"""Checks a guest image made by make-image.py.

    uv run dsa/guest/image/check-image.py dsa/target/image/17.0.0-16373615 .scratch/image-keys

It fails when the shell apps are still there, when the build is still debuggable, when an
app is not signed with one of the keys in the key folder, or when the files do not match
image.json.

The signatures are read from the apps themselves. sign_target_files_apks copies
META/apkcerts.txt over unchanged, so that file still names AOSP's keys after signing.
"""

import base64
import io

import hashlib
import json
import re
import subprocess
import sys
import zipfile
from pathlib import Path

image = Path(sys.argv[1])
keys = Path(sys.argv[2])
ours = [base64.b64decode("".join(l for l in p.read_text().splitlines() if "-----" not in l))
        for p in sorted(keys.glob("*.x509.pem"))]
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
    presigned = set(re.findall(r'name="([^"]+)" certificate="PRESIGNED"', z.read("META/apkcerts.txt").decode()))
    unsigned = []
    apks = [n for n in names if n.endswith(".apk") and n.rsplit("/", 1)[-1] not in presigned]
    for name in apks:
        data = z.read(name)
        # v2 and v3 signatures keep the certificate uncompressed in the signing block;
        # a v1-only app keeps it in a compressed META-INF entry.
        if any(cert in data for cert in ours):
            continue
        with zipfile.ZipFile(io.BytesIO(data)) as apk:
            blocks = [apk.read(n) for n in apk.namelist()
                      if re.fullmatch(r"META-INF/[^/]+\.(RSA|DSA|EC)", n)]
        if not any(cert in b for b in blocks for cert in ours):
            unsigned.append(name)
    print(f"{len(apks) - len(unsigned)} of {len(apks)} apps are signed with our keys")
    if unsigned:
        problems.append(f"{len(unsigned)} apps are not signed with our keys, e.g. {unsigned[:3]}")
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
