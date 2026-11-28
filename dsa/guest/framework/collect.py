"""Collects what the framework build made into the folder make-image.py --framework takes.

    uv run dsa/guest/framework/collect.py --android android17 --arch x86_64 \\
        --product-out <tree>/out/target/product/dsa_framework --top <tree> --out <folder>

The folder holds each file under its target_files name (SYSTEM/bin/surfaceflinger, ...) and
framework.json, which says what they are: the Android version and the published build they
were made from, the build's version (framework_lock.py version), the architecture, and each
file's size and SHA-256. make-image.py takes the files only into an image of that same build
and architecture, and check-image.py finds the same hashes in the image.

Beside the framework's files (framework_lock.OUTPUTS) it holds DSA's KeyMint: the files the build installs
(framework_lock.KEYMINT_OUTPUTS), and the VINTF fragments and the feature file the KeyMint APEX
carries, which come from the tree as it was fetched (framework_lock.KEYMINT_SOURCES; hence
--top). With --services, it also holds services.jar and framework.jar from the services build
(framework_lock.JAVA_OUTPUTS), which make-image.py takes in place of the published build's. The
two come together or not at all: the window manager in one calls the other's new code.

The Framework build makes the native files and services.jar in separate jobs, which run at the
same time, so it collects the native files first and adds the jar to that folder afterwards:

    uv run dsa/guest/framework/collect.py --add-services <services folder> --out <folder>

The folder is then the same as one collected with --services.
"""

import argparse
import hashlib
import json
import shutil
import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import framework_lock  # noqa: E402

MANIFEST = "framework.json"
# The ELF machine of a 64-bit binary for each guest architecture.
ELF_MACHINES = {"x86_64": 62, "arm64": 183}


def elf_machine(data):
    """The e_machine of a little-endian 64-bit ELF file, or None for anything else."""
    if len(data) < 20 or data[:4] != b"\x7fELF" or data[4] != 2 or data[5] != 1:
        return None
    return int.from_bytes(data[18:20], "little")


def sha256(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def is_dex_jar(path):
    """Whether `path` is a jar with dex code (classes.dex), as the services build makes it."""
    try:
        with zipfile.ZipFile(path) as z:
            return "classes.dex" in z.namelist() and z.read("classes.dex")[:4] == b"dex\n"
    except (zipfile.BadZipFile, OSError):
        return False


def take_services(services, out, files):
    """Copies services.jar from `services`, the folder the services build left it in (under its
    target_files name), into `out`, and describes it in `files`."""
    for name in framework_lock.JAVA_OUTPUTS:
        source = Path(services) / name
        if not source.is_file():
            raise SystemExit(f"the services build left no {name} (looked in {services})")
        if not is_dex_jar(source):
            raise SystemExit(f"{source} is not a jar with dex code")
        target = Path(out) / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        files[name] = {"size": target.stat().st_size, "sha256": sha256(target)}


def add_services(out, services):
    """Adds services.jar from `services` to `out`, a folder collect() already made without it,
    and returns its new description. The folder ends up as collect() with `services` makes it,
    framework.json included, so the native files and the jar can come from separate jobs."""
    out = Path(out)
    manifest = out / MANIFEST
    if not manifest.is_file():
        raise SystemExit(f"{out} has no {MANIFEST}: collect the build's files there first")
    description = json.loads(manifest.read_text())
    taken = [name for name in framework_lock.JAVA_OUTPUTS if name in description["files"]]
    if taken:
        raise SystemExit(f"{out} already holds {', '.join(taken)}")
    take_services(services, out, description["files"])
    manifest.write_text(json.dumps(description, indent=2) + "\n")
    return description


def collect(version, arch, product_out, out, sources=None, patches=None, top=None, services=None):
    """Copies the build's outputs from `product_out`, and the KeyMint's files the tree `top`
    has, into `out` and writes framework.json; returns what it wrote there. With `services`, the
    folder the services build left services.jar in (under its target_files name), it takes that
    too: a dex file is the same for every architecture."""
    if arch not in ELF_MACHINES:
        raise SystemExit(f"the architecture must be one of {', '.join(ELF_MACHINES)}, not {arch}")
    if top is None:
        raise SystemExit("the KeyMint's VINTF fragments and feature file come from the tree: give --top")
    product_out, out, top = Path(product_out), Path(out), Path(top)
    files = {}
    for name, installed in {**framework_lock.OUTPUTS, **framework_lock.KEYMINT_OUTPUTS}.items():
        source = product_out / installed
        if not source.is_file():
            raise SystemExit(f"the build did not make {installed} (looked in {product_out})")
        with open(source, "rb") as f:
            found = elf_machine(f.read(64))
        if found != ELF_MACHINES[arch]:
            raise SystemExit(f"{installed} is built for ELF machine {found}, not {arch} ({ELF_MACHINES[arch]})")
        target = out / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        files[name] = {"size": target.stat().st_size, "sha256": sha256(target)}
    for name, source_path in framework_lock.KEYMINT_SOURCES.items():
        source = top / source_path
        if not source.is_file():
            raise SystemExit(f"the tree has no {source_path} (looked in {top})")
        target = out / name
        target.parent.mkdir(parents=True, exist_ok=True)
        shutil.copyfile(source, target)
        files[name] = {"size": target.stat().st_size, "sha256": sha256(target)}
    if services is not None:
        take_services(services, out, files)
    description = {
        "android": version["android"],
        "android_major": version["android_major"],
        "build_id": version["build_id"],
        "target": version["target"],
        "release": version["release"],
        "arch": arch,
        "version": framework_lock.build_version(version),
        "files": files,
    }
    if sources is not None:
        description["sources"] = sources
    if patches is not None:
        description["patches"] = patches
    (out / MANIFEST).write_text(json.dumps(description, indent=2) + "\n")
    return description


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--android")
    parser.add_argument("--arch")
    parser.add_argument("--product-out", type=Path)
    parser.add_argument("--top", type=Path,
                        help="the tree: what was fetched and patched, and the KeyMint's VINTF fragments")
    parser.add_argument("--out", type=Path, required=True)
    parser.add_argument("--services", type=Path,
                        help="the folder the services build left services.jar in, when it passed")
    parser.add_argument("--add-services", type=Path, metavar="SERVICES",
                        help="add services.jar from this folder to the one collect.py already made in --out, "
                             "with no other option")
    args = parser.parse_args()
    if args.add_services is not None:
        if any(v is not None for v in (args.android, args.arch, args.product_out, args.top, args.services)):
            parser.error("--add-services takes only --out")
        description = add_services(args.out, args.add_services)
        print(json.dumps({k: v for k, v in description.items() if k not in ("sources", "patches")}, indent=2))
        return
    missing = [f"--{n.replace('_', '-')}" for n in ("android", "arch", "product_out", "top")
               if getattr(args, n) is None]
    if missing:
        parser.error("the build's files need " + ", ".join(missing))
    version = framework_lock.version_entry(framework_lock.load(), args.android)
    for name in (".dsa-sources.json", ".dsa-patches.json"):
        if not (args.top / name).is_file():
            raise SystemExit(f"{args.top / name} is missing: the tree was not fetched and patched "
                             "by fetch-source.py and apply-patches.py")
    sources = json.loads((args.top / ".dsa-sources.json").read_text())
    patches = json.loads((args.top / ".dsa-patches.json").read_text())
    description = collect(version, args.arch, args.product_out, args.out, sources, patches, args.top,
                          args.services)
    print(json.dumps({k: v for k, v in description.items() if k not in ("sources", "patches")}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
