"""Tests for making a YUV ColorBuffer linear.

What a guest CPU writes into a YUV buffer (a video's pictures) is copied into its ColorBuffer's
image, and SurfaceFlinger samples it through an image of its own on the same memory, made on the
guest's VkDevice. Both were optimal. On the owner's PC (Intel Arc, 0.2.4-dev.15, with patches
0030 and 0031) a YouTube video still showed blocks of light purple and yellow-green in the flat
areas of its picture, with the decoder's planes checked whole: a compressed surface read as
uncompressed. The guest takes its image from the external queue family once, when it first
imports the buffer, so 0031's release after every copy is never matched by an acquire.

Patch 0032 makes such a ColorBuffer linear where the device can sample, copy and export it that
way, and makes the guest's image of it the same way, since a linear image is never compressed."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0032-*.patch"))
OPERATIONS = "host/vulkan/VkCommonOperations.cpp"
HEADER = "host/vulkan/VkCommonOperations.h"
DECODER = "host/vulkan/VkDecoderGlobalState.cpp"


def sides(patch, name):
    """The added and removed lines of one file."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
            removed = [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")]
            return "\n".join(added), "\n".join(removed)
    raise AssertionError(f"{patch.name} does not change {name}")


def function(text, signature):
    """The added lines from the function that starts with `signature` on (it is added last)."""
    return text[text.index(signature):]


class LinearYuvPatchTests(unittest.TestCase):
    def setUp(self):
        self.operations, self.operations_removed = sides(PATCH, OPERATIONS)
        self.header, _ = sides(PATCH, HEADER)
        self.decoder, self.decoder_removed = sides(PATCH, DECODER)
        self.tiling = function(self.operations,
                               "VkImageTiling VkEmulation::colorBufferTiling(VkFormat format) const {")

    def test_pr4_only_the_color_buffer_and_its_import_change(self):
        text = PATCH.read_text(encoding="utf-8")
        files = re.findall(r"^diff --git a/(\S+) ", text, flags=re.M)
        self.assertEqual(files, [OPERATIONS, HEADER, DECODER])
        self.assertIn("VkImageTiling colorBufferTiling(VkFormat format) const;", self.header)

    def test_pr4_only_the_8_bit_420_yuv_formats_are_made_linear(self):
        self.assertIn("if (format != VK_FORMAT_G8_B8R8_2PLANE_420_UNORM &&\n"
                      "        format != VK_FORMAT_G8_B8_R8_3PLANE_420_UNORM) {\n"
                      "        return VK_IMAGE_TILING_OPTIMAL;", self.tiling)
        self.assertIn("return linear ? VK_IMAGE_TILING_LINEAR : VK_IMAGE_TILING_OPTIMAL;",
                      self.tiling)

    def test_nfr5_linear_only_where_the_device_samples_copies_and_exports_it(self):
        # A ColorBuffer the device cannot make would end the renderer, so linear tiling is
        # used only when every use of a YUV ColorBuffer works with it.
        for feature in ("VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT",
                        "VK_FORMAT_FEATURE_MIDPOINT_CHROMA_SAMPLES_BIT",
                        "VK_FORMAT_FEATURE_TRANSFER_SRC_BIT",
                        "VK_FORMAT_FEATURE_TRANSFER_DST_BIT"):
            self.assertIn(feature, self.tiling)
        self.assertIn("linearTilingFeatures & kNeeded) !=\n                kNeeded", self.tiling)
        self.assertIn("supportInfo.tiling != VK_IMAGE_TILING_LINEAR", self.tiling)
        self.assertIn("(mDeviceInfo.supportsExternalMemoryExport && "
                      "!supportInfo.supportsExternalMemory)", self.tiling)
        self.assertIn("const bool linear = sampled && written && read;", self.tiling)

    def test_pr4_the_renderer_says_once_which_tiling_it_chose(self):
        self.assertIn("std::call_once(", self.tiling)
        self.assertIn("are linear", self.tiling)
        self.assertIn("are optimal", self.tiling)

    def test_pr4_a_device_local_color_buffer_takes_that_tiling(self):
        self.assertIn(": colorBufferTiling(vkFormat);", self.operations)
        self.assertIn(": VK_IMAGE_TILING_OPTIMAL;", self.operations_removed)
        # Host-visible ColorBuffers stay linear, as before.
        self.assertNotIn("VK_MEMORY_PROPERTY_HOST_VISIBLE_BIT", self.operations_removed)

    def test_pr4_the_guests_image_of_the_buffer_is_made_the_same_way(self):
        # The image that imports the ColorBuffer's memory must have its tiling, or it reads
        # the planes in another layout; it is set before its other parameters are taken from
        # what the ColorBuffer is made with.
        self.assertIn("if (imageCreateInfo.tiling == VK_IMAGE_TILING_OPTIMAL) {", self.decoder)
        self.assertIn("imageCreateInfo.tiling = m_vkEmulation->colorBufferTiling(resolvedFormat);",
                      self.decoder)
        self.assertEqual(self.decoder_removed, "")
        text = PATCH.read_text(encoding="utf-8")
        self.assertLess(text.index("m_vkEmulation->colorBufferTiling(resolvedFormat)"),
                        text.index(" colorBufferVkImageCi = m_vkEmulation->"
                                   "generateColorBufferVkImageCreateInfo("))

    def test_pr4_the_fetch_applies_it_after_the_release_it_replaces(self):
        names = sorted(p.name for p in (HERE / "patches").glob("00*.patch"))
        self.assertEqual(names.index(PATCH.name),
                         names.index(next(n for n in names if n.startswith("0031-"))) + 1)
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0032", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
