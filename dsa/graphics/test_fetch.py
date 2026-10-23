"""Tests for the reproducible gfxstream source checkout."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
FETCH = HERE / "fetch.sh"


class GfxstreamFetchTests(unittest.TestCase):
    def test_pr3_fetches_the_pinned_upstream_revision_before_applying_dsa_patches(self):
        script = FETCH.read_text()
        self.assertIn("d047a57228332d995d36600792fa9ccc26cf8ae6", script)
        self.assertIn(
            "https://android.googlesource.com/platform/hardware/google/gfxstream",
            script,
        )
        self.assertIn("git -C \"$source\" checkout --detach \"$revision\"", script)
        self.assertIn("git -C \"$source\" apply --recount --check", script)
        self.assertIn("git -C \"$source\" apply --recount", script)

    def test_pr3_fetches_pinned_mesa_revision(self):
        script = FETCH.read_text()
        self.assertIn("224e91e39836d3ca31f80df2e0379e55c70574f8", script)
        self.assertIn("git -C \"$mesa_dir\" checkout --detach \"$mesa_revision\"", script)
        self.assertIn("git -C \"$mesa_dir\" reset --hard \"$mesa_revision\"", script)

    def test_pr3_fetches_shallow_guest_dependencies(self):
        script = FETCH.read_text()
        self.assertIn("https://android.googlesource.com/platform/external/mesa3d", script)
        self.assertIn("https://android.googlesource.com/platform/external/minigbm", script)
        self.assertIn("https://android.googlesource.com/platform/hardware/libhardware", script)
        self.assertIn("https://android.googlesource.com/platform/system/libbase", script)
        self.assertIn("https://android.googlesource.com/platform/system/core", script)
        self.assertIn("https://android.googlesource.com/platform/system/logging", script)
        self.assertIn("src/vulkan", script)
        self.assertIn("cros_gralloc", script)


if __name__ == "__main__":
    unittest.main()

