"""Collects what the framework build made into the folder make-image.py --framework takes.

    uv run dsa/guest/framework/collect.py --android android17 --arch x86_64 \\
        --product-out <tree>/out/target/product/dsa_framework --top <tree> --out <folder>

The folder holds each file under its target_files name (SYSTEM/bin/surfaceflinger, ...) and
framework.json, which says what they are: the Android version and the published build they
were made from, the build's version (framework_lock.py version), the architecture, and each
file's size and SHA-256. make-image.py takes the files only into an image of that same build
and architecture, and check-image.py finds the same hashes in the image.
"""

import argparse
import hashlib
import json
import shutil
import sys
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


def collect(version, arch, product_out, out, sources=None, patches=None):
    """Copies the build's outputs from `product_out` into `out` and writes framework.json;
    returns what it wrote there."""
    if arch not in ELF_MACHINES:
        raise SystemExit(f"the architecture must be one of {', '.join(ELF_MACHINES)}, not {arch}")
    product_out, out = Path(product_out), Path(out)
    files = {}
    for name, installed in framework_lock.OUTPUTS.items():
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
    parser.add_argument("--android", required=True)
    parser.add_argument("--arch", required=True)
    parser.add_argument("--product-out", type=Path, required=True)
    parser.add_argument("--top", type=Path, help="the tree, for what was fetched and patched")
    parser.add_argument("--out", type=Path, required=True)
    args = parser.parse_args()
    version = framework_lock.version_entry(framework_lock.load(), args.android)
    sources = patches = None
    if args.top:
        for name in (".dsa-sources.json", ".dsa-patches.json"):
            if not (args.top / name).is_file():
                raise SystemExit(f"{args.top / name} is missing: the tree was not fetched and patched "
                                 "by fetch-source.py and apply-patches.py")
        sources = json.loads((args.top / ".dsa-sources.json").read_text())
        patches = json.loads((args.top / ".dsa-patches.json").read_text())
    description = collect(version, args.arch, args.product_out, args.out, sources, patches)
    print(json.dumps({k: v for k, v in description.items() if k not in ("sources", "patches")}, indent=2))


if __name__ == "__main__":
    sys.exit(main())
