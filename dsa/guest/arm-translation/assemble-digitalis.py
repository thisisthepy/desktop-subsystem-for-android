"""Assembles the Digitalis translator's binary bundle from a reduced AOSP build and DSA's
pinned images, for make-package.py to turn into an ARM translation package.

The bundle is the file list Digitalis itself ships (BERBERIS_DISTRIBUTION_ARTIFACTS_ARM64 in
frameworks/libs/binary_translation/berberis_config.mk): the translator and its proxy
libraries (x86_64), the program runners, the binfmt_misc rules and linker configuration, and
the arm64 guest libraries the translated apps link against. Most of it comes from the build.
The guest libraries that are plain arm64 builds of AOSP libraries no proxy stands in for
(ICU, BoringSSL, SQLite) come instead from the arm64 image DSA pins for the same Android
version, which is built from the same sources: it saves building them, and they are the same
code an arm64 phone of that release runs. The libraries the native bridge replaces (libc,
libm, libEGL and the other proxied ones, the linker, app_process) are always built, because
the arm64 image's own are the real ones and would bypass the translator.

    assemble-digitalis.py targets --config-mk <berberis_config.mk> --product-out <rel path>
        the build's install paths of the files it must build, one per line
    assemble-digitalis.py framework-libs --target-files <x86_64 target_files.zip> --out <dir>
        the x86_64 framework libraries the proxies link against
    assemble-digitalis.py guest-libs --target-files <arm64 target_files.zip> --out <dir>
        the arm64 guest libraries the bundle takes from the arm64 image
    assemble-digitalis.py collect --config-mk <mk> --product-out <dir> --guest-libs <dir>
        --out <bundle dir> [--license <file>] [--sources <json>]
        the bundle: <out>/system/... as make-package.py takes it

framework-libs and guest-libs need debugfs (e2fsprogs) or fsck.erofs (erofs-utils) for the
libraries that live in APEXes.
"""

import argparse
import io
import json
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import zipfile
from pathlib import Path

ARTIFACTS_VAR = "BERBERIS_DISTRIBUTION_ARTIFACTS_ARM64"
GUEST_DIRS = ("system/lib64/arm64/", "system/bin/arm64/")
# Guest libraries taken from the arm64 image, and the APEX each lives in when it is not in
# the system partition's lib64.
GUEST_LIBS_FROM_IMAGE = {
    "libandroidicu.so": "com.android.i18n",
    "libicu.so": "com.android.i18n",
    "libicui18n.so": "com.android.i18n",
    "libicuuc.so": "com.android.i18n",
    "libcrypto.so": "com.android.conscrypt",
    "libssl.so": "com.android.conscrypt",
    "libsqlite.so": None,
}
# The x86_64 framework libraries the proxy libraries link against, taken from the x86_64
# image. .github/scripts/translator/device/prebuilts/Android.bp names the same ones.
FRAMEWORK_LIBS = {
    "libEGL.so": None,
    "libGLESv1_CM.so": None,
    "libGLESv2.so": None,
    "libGLESv3.so": None,
    "libOpenMAXAL.so": None,
    "libOpenSLES.so": None,
    "libaaudio.so": None,
    "libamidi.so": None,
    "libandroid.so": None,
    "libandroid_runtime.so": None,
    "libbinder_ndk.so": None,
    "libcamera2ndk.so": None,
    "libjnigraphics.so": None,
    "libmediandk.so": None,
    "libnativewindow.so": None,
    "libneuralnetworks.so": "com.android.neuralnetworks",
    "libvulkan.so": None,
    "libwebviewchromium_plat_support.so": None,
}
EM_X86_64 = 0x3E
EM_AARCH64 = 0xB7
EROFS_MAGIC = 0xE0F5E1E2
EXT4_MAGIC = 0xEF53


def make_list(text, var):
    """The words of the first non-empty `var :=` assignment in a makefile, following
    continuation lines."""
    lines = text.splitlines()
    for index, line in enumerate(lines):
        match = re.match(rf"^{re.escape(var)}\s*:=\s*(.*)$", line)
        if not match:
            continue
        words, rest = [], match.group(1)
        while True:
            more = rest.endswith("\\")
            words.extend(rest.rstrip("\\").split())
            if not more or index + 1 >= len(lines):
                break
            index += 1
            rest = lines[index].strip()
        if words:
            return words
    raise SystemExit(f"the makefile has no {var}")


def artifacts(config_mk):
    found = make_list(Path(config_mk).read_text(), ARTIFACTS_VAR)
    bad = [a for a in found if not a.startswith("system/") or ".." in a.split("/")]
    if bad:
        raise SystemExit(f"{ARTIFACTS_VAR} names files outside system/: {bad}")
    return found


def from_image(artifact):
    """The library name when `artifact` is a guest library the arm64 image provides."""
    if artifact.startswith("system/lib64/arm64/"):
        name = artifact[len("system/lib64/arm64/"):]
        if name in GUEST_LIBS_FROM_IMAGE:
            return name
    return None


def build_targets(config_mk, product_out):
    """The install paths the build must produce: every artifact the image does not provide."""
    return [f"{product_out.rstrip('/')}/{a}" for a in artifacts(config_mk) if not from_image(a)]


# --- target_files and APEX access -----------------------------------------------------

def is_symlink(info):
    return stat.S_ISLNK(info.external_attr >> 16)


def apex_payload(apex_bytes):
    """The payload file system image of an APEX, or of a compressed APEX (capex)."""
    with zipfile.ZipFile(io.BytesIO(apex_bytes)) as outer:
        names = outer.namelist()
        if "original_apex" in names:
            return apex_payload(outer.read("original_apex"))
        return outer.read("apex_payload.img")


def payload_kind(image):
    if len(image) >= 1028 and int.from_bytes(image[1024:1028], "little") == EROFS_MAGIC:
        return "erofs"
    if len(image) >= 1082 and int.from_bytes(image[1080:1082], "little") == EXT4_MAGIC:
        return "ext4"
    return None


def read_from_payload(image, path):
    """The file at `path` (such as lib64/libicuuc.so) in an APEX payload image."""
    kind = payload_kind(image)
    with tempfile.TemporaryDirectory() as tmp:
        payload = Path(tmp) / "payload.img"
        payload.write_bytes(image)
        if kind == "ext4":
            out = Path(tmp) / "out"
            subprocess.run(["debugfs", "-R", f"dump /{path} {out}", str(payload)],
                           check=True, capture_output=True)
            if not out.is_file() or out.stat().st_size == 0:
                raise FileNotFoundError(path)
            return out.read_bytes()
        if kind == "erofs":
            tree = Path(tmp) / "tree"
            tree.mkdir()
            subprocess.run(["fsck.erofs", f"--extract={tree}", str(payload)],
                           check=True, capture_output=True)
            found = tree / path
            if not found.is_file():
                raise FileNotFoundError(path)
            return found.read_bytes()
    raise SystemExit("an APEX payload is neither ext4 nor erofs")


class TargetFiles:
    """Reads libraries out of an AOSP target_files archive, from the system partition's lib64
    or from an APEX in it."""

    def __init__(self, path, payload_reader=read_from_payload):
        self.zip = zipfile.ZipFile(path)
        self.names = set(self.zip.namelist())
        self.payload_reader = payload_reader
        self.payloads = {}

    def apex(self, module):
        if module not in self.payloads:
            for suffix in (".apex", ".capex"):
                name = f"SYSTEM/apex/{module}{suffix}"
                if name in self.names:
                    self.payloads[module] = apex_payload(self.zip.read(name))
                    break
            else:
                raise SystemExit(f"{self.zip.filename} has no APEX {module}")
        return self.payloads[module]

    def library(self, name, apex=None):
        member = f"SYSTEM/lib64/{name}"
        if member in self.names:
            info = self.zip.getinfo(member)
            if not is_symlink(info):
                return self.zip.read(member)
            target = self.zip.read(member).decode()
            found = re.match(r"^/apex/([^/]+)/(.+)$", target)
            if not found:
                raise SystemExit(f"{member} links to {target}, which is not in an APEX")
            return self.payload_reader(self.apex(found.group(1)), found.group(2))
        if apex is None:
            raise SystemExit(f"{self.zip.filename} has no {member}")
        return self.payload_reader(self.apex(apex), f"lib64/{name}")


def elf_machine(data):
    if data[:4] != b"\x7fELF" or len(data) < 20:
        return None
    return int.from_bytes(data[18:20], "little")


def check_machine(rel, data):
    """Guest files are arm64; the translator, its proxies and runners are x86_64."""
    machine = elf_machine(data)
    if rel.startswith(GUEST_DIRS):
        if machine != EM_AARCH64:
            raise SystemExit(f"{rel} is a guest file but is not arm64")
    elif rel.endswith(".so") or rel.startswith("system/bin/"):
        if machine != EM_X86_64:
            raise SystemExit(f"{rel} is a host file but is not x86_64")


def collect(config_mk, product_out, guest_libs, out, license_file=None, sources=None):
    product_out, guest_libs, out = Path(product_out), Path(guest_libs), Path(out)
    missing = []
    taken = {"build": 0, "image": 0}
    for rel in artifacts(config_mk):
        name = from_image(rel)
        source = guest_libs / name if name else product_out / rel
        if not source.is_file():
            missing.append(rel if not name else f"{rel} (from the arm64 image)")
            continue
        data = source.read_bytes()
        taken["image" if name else "build"] += 1
        check_machine(rel, data)
        target = out / rel
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_bytes(data)
        if rel.startswith("system/bin/"):
            target.chmod(0o755)
    if missing:
        raise SystemExit("the bundle lacks " + ", ".join(missing))
    notes = out / "system/etc/digitalis"
    notes.mkdir(parents=True, exist_ok=True)
    if license_file:
        shutil.copy(license_file, notes / "LICENSE")
    if sources:
        shutil.copy(sources, notes / "SOURCES.json")
    print(f"{out}: {taken['build']} files from the build, {taken['image']} from the arm64 image")
    return taken


def take_libraries(target_files, libraries, machine, out, payload_reader=read_from_payload):
    """Writes each of `libraries` ({name: APEX or None}) from an image into `out`, after
    checking it is built for `machine`."""
    out = Path(out)
    out.mkdir(parents=True, exist_ok=True)
    image = TargetFiles(target_files, payload_reader)
    for name, apex in libraries.items():
        data = image.library(name, apex)
        if elf_machine(data) != machine:
            raise SystemExit(f"{name} in {target_files} is not built for the expected architecture")
        (out / name).write_bytes(data)
    print(f"{out}: {len(libraries)} libraries from {Path(target_files).name}")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = parser.add_subparsers(dest="command", required=True)
    targets = sub.add_parser("targets")
    targets.add_argument("--config-mk", type=Path, required=True)
    targets.add_argument("--product-out", required=True)
    for command in ("framework-libs", "guest-libs"):
        libs = sub.add_parser(command)
        libs.add_argument("--target-files", type=Path, required=True)
        libs.add_argument("--out", type=Path, required=True)
    bundle = sub.add_parser("collect")
    bundle.add_argument("--config-mk", type=Path, required=True)
    bundle.add_argument("--product-out", type=Path, required=True)
    bundle.add_argument("--guest-libs", type=Path, required=True)
    bundle.add_argument("--out", type=Path, required=True)
    bundle.add_argument("--license", type=Path)
    bundle.add_argument("--sources", type=Path)
    args = parser.parse_args()
    if args.command == "targets":
        print("\n".join(build_targets(args.config_mk, args.product_out)))
    elif args.command == "framework-libs":
        take_libraries(args.target_files, FRAMEWORK_LIBS, EM_X86_64, args.out)
    elif args.command == "guest-libs":
        take_libraries(args.target_files, GUEST_LIBS_FROM_IMAGE, EM_AARCH64, args.out)
    else:
        collect(args.config_mk, args.product_out, args.guest_libs, args.out,
                args.license, args.sources)


if __name__ == "__main__":
    sys.exit(main())
