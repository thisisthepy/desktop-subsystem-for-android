# /// script
# requires-python = ">=3.11"
# ///
"""Tests for scripts/tests/kernel_source_release.py."""

import importlib.util
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("kernel_source_release", HERE / "kernel_source_release.py")
ksr = importlib.util.module_from_spec(spec)
spec.loader.exec_module(ksr)

GOOD = """\
name: Release
jobs:
  kernel:
    steps:
      - name: Build
        run: build.sh
      - uses: actions/upload-artifact@v4
        with:
          name: guest-kernel
          path: |
            out/bzImage
            out/kernel-source.tar.gz
  image:
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: guest-kernel
      - name: Publish the image
        run: |
          files+=(out/kernel-source.tar.gz)
          gh release create "$tag" "${files[@]}"
  list:
    steps:
      - name: Publish the list
        run: gh release upload images images.json
  installer:
    steps:
      - uses: actions/download-artifact@v4
        with:
          name: guest-kernel
      - name: Publish the installer
        run: gh release create "v$version" setup.exe out/kernel-source.tar.gz
      - name: Publish the program list
        run: gh release upload images program.json
"""


class Workflow(unittest.TestCase):
    def test_nfr7_release_with_kernel_and_source_passes(self):
        self.assertEqual(ksr.problems(GOOD), [])

    def test_nfr7_kernel_artifact_without_source_fails(self):
        bad = GOOD.replace("            out/kernel-source.tar.gz\n", "", 1)
        self.assertEqual(ksr.problems(bad), ["job kernel: uploads guest-kernel without kernel-source.tar.gz"])

    def test_nfr7_image_release_without_source_fails(self):
        bad = GOOD.replace("          files+=(out/kernel-source.tar.gz)\n", "")
        self.assertEqual(ksr.problems(bad),
                         ["job image: publishes a release with the kernel but not kernel-source.tar.gz"])

    def test_nfr7_installer_release_without_source_fails(self):
        bad = GOOD.replace("setup.exe out/kernel-source.tar.gz", "setup.exe")
        self.assertEqual(ksr.problems(bad),
                         ["job installer: publishes a release with the kernel but not kernel-source.tar.gz"])

    def test_nfr7_the_real_release_workflow_ships_the_kernel_source(self):
        self.assertEqual(ksr.problems(ksr.DEFAULT.read_text(encoding="utf-8")), [])


if __name__ == "__main__":
    unittest.main()
