"""Tests for host patch 0047: a window image the renderer uses natively has no staging buffer.

gfxstream gave every Android native buffer image (each window buffer a guest app draws into) a
host-visible staging buffer of the image's size, mapped for the image's whole life. Only a
release that copies the image back to the CPU reads it, and a Vulkan-only renderer, which DSA's
is, uses every such image natively and never copies it back. So the buffers held nothing but
memory: the CPU-mapped memory of each app's device in the renderer's memory report."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


def removed(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("-") and not l.startswith("---"))


class NativeImageStagingPatchTests(unittest.TestCase):
    def setUp(self):
        self.patch = read(next(PATCHES.glob("0047-*.patch")))

    def test_nfr1_a_native_window_image_allocates_no_staging_buffer(self):
        self.assertIn("+++ b/host/vulkan/VkAndroidNativeBuffer.cpp", self.patch)
        self.assertIn("if (!out->mUseVulkanNativeImage) {", added(self.patch))
        # The block that made, bound and mapped the buffer is what the condition now guards:
        # it used to open unconditionally.
        self.assertEqual(removed(self.patch).strip(), "{")
        self.assertIn("VkBufferCreateInfo stagingBufferCreateInfo = {", self.patch)

    def test_nfr1_the_patch_says_why_and_cites_no_planning_document(self):
        body = added(self.patch)
        self.assertIn("on_vkQueueSignalReleaseImageANDROID", body)
        for cited in ("SPEC", "INTENT", "NFR-", "FR-"):
            self.assertNotIn(cited, body)

    def test_pr3_every_context_line_starts_with_a_space(self):
        in_hunk = False
        for line in self.patch.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_pr3_the_fetch_applies_0047_after_0046(self):
        self.assertIn(" 0046 0047", read(HOST / "fetch.sh"))
        fetch = read(HOST / "fetch.ps1")
        self.assertIn('-Filter "0047-*.patch"', fetch)
        self.assertLess(fetch.index('"0046-*.patch"'), fetch.index('"0047-*.patch"'))


if __name__ == "__main__":
    unittest.main()
