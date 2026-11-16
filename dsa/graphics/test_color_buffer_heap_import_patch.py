"""Tests for host patch 0043: a ColorBuffer's Metal heap is imported without a dedicated image.

On macOS the renderer runs on MoltenVK and imports the heap of a ColorBuffer into every buffer a
guest process makes an image of. The guest driver names no image to the allocation when the
memory type is host-visible, which is the one it picks on Apple silicon, so the renderer made
one up: the ColorBuffer's own image. MoltenVK then refused to bind the guest's image to the
memory ("A dedicated allocation may only be used with the resource it was dedicated to",
VK_ERROR_OUT_OF_DEVICE_MEMORY). ANGLE turned that into EGL_BAD_PARAMETER, so every buffer
SurfaceFlinger imported failed with "Could not create EGL image, err = (0x300c)" and every app
window stayed black."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


class ColorBufferHeapImportPatchTests(unittest.TestCase):
    def setUp(self):
        self.text = read(next(PATCHES.glob("0043-*.patch")))
        lines = self.text.splitlines()
        self.added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
        self.removed = [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")]

    def test_pr3_the_import_no_longer_dedicates_the_heap_to_the_color_buffers_own_image(self):
        removed = "\n".join(self.removed)
        self.assertIn("m_vkEmulation->getColorBufferVkImage(importCbInfoPtr->colorBuffer)", removed)
        self.assertIn("shouldUseDedicatedAllocInfo = true;", removed)
        self.assertNotIn("getColorBufferVkImage", "\n".join(self.added))

    def test_pr3_an_import_that_names_no_image_is_not_dedicated(self):
        added = "\n".join(self.added)
        self.assertIn("localDedicatedAllocInfo.image == VK_NULL_HANDLE", added)
        self.assertIn("shouldUseDedicatedAllocInfo = false;", added)

    def test_pr3_the_reason_is_in_the_code(self):
        added = "\n".join(self.added)
        self.assertIn("A dedicated allocation may only be used with the", added)

    def test_pr3_every_hunk_line_starts_with_a_diff_marker(self):
        in_hunk = False
        for line in self.text.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_pr3_the_fetch_applies_0043_after_0042(self):
        self.assertIn(" 0042 0043 ", read(HOST / "fetch.sh"))
        self.assertIn('-Filter "0043-*.patch"', read(HOST / "fetch.ps1"))


if __name__ == "__main__":
    unittest.main()
