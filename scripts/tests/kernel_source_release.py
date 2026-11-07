# /// script
# requires-python = ">=3.11"
# ///
"""Checks that the release workflow ships the kernel's source wherever it ships the kernel.

The GPL-2.0 kernel goes out with its corresponding source beside it, so in the workflow:
- every upload of a kernel artifact (`guest-kernel`, `kernel-arm64`) also carries
  kernel-source.tar.gz, and
- every job that takes a kernel artifact and publishes a release names
  kernel-source.tar.gz among what it publishes.

Usage: kernel_source_release.py [workflow]. Prints each problem and exits 1 if there is one.
"""

import re
import sys
from pathlib import Path

ASSET = "kernel-source.tar.gz"
# The artifacts that carry a kernel: the x86_64 bzImage and the arm64 Image.
KERNELS = ("guest-kernel", "kernel-arm64")
DEFAULT = Path(__file__).resolve().parents[2] / ".github/workflows/release-image.yml"


def jobs(text: str) -> dict:
    """The workflow's jobs, by id, each as its own text."""
    body = text.split("\njobs:\n", 1)[1] if "\njobs:\n" in text else ""
    found = {}
    current = None
    for line in body.splitlines():
        m = re.match(r"^  ([A-Za-z0-9_-]+):\s*$", line)
        if m:
            current = m.group(1)
            found[current] = []
        elif current:
            found[current].append(line)
    return {k: "\n".join(v) for k, v in found.items()}


def steps(job: str) -> list:
    return re.split(r"\n\s*- (?=uses:|name:)", job)


def problems(text: str) -> list:
    out = []
    for name, job in jobs(text).items():
        takes_kernel = False
        for step in steps(job):
            kernel = next((k for k in KERNELS if k in step and "name:" in step), None)
            if not kernel:
                continue
            if "upload-artifact" in step and ASSET not in step:
                out.append(f"job {name}: uploads {kernel} without {ASSET}")
            if "download-artifact" in step:
                takes_kernel = True
        if not takes_kernel:
            continue
        for step in steps(job):
            # The "images" release holds only the signed lists, never a kernel.
            targets = re.findall(r"gh release (?:create|upload) (\S+)", step)
            if any(t != "images" for t in targets) and ASSET not in step:
                out.append(f"job {name}: publishes a release with the kernel but not {ASSET}")
    return out


def main() -> int:
    path = Path(sys.argv[1]) if len(sys.argv) > 1 else DEFAULT
    found = problems(path.read_text(encoding="utf-8"))
    for p in found:
        print(f"error: {p}", file=sys.stderr)
    if found:
        return 1
    print(f"ok    every release that ships the kernel ships {ASSET}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
