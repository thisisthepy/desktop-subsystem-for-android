"""Tests for host patch 0045: the renderer never composes a YUV ColorBuffer that holds no
finished upload.

YouTube Shorts flickered green while the host decoder and the hwcomposer's upload both reported
every picture whole. The compositor sampled a YUV ColorBuffer whatever its uploads had done: one
no upload had finished in (new, or made again after a GPU reset), one a failed upload left
part-written, or one another guest process had overwritten with planes its producer had not
written yet. Zeros read as YUV are solid green. The rules are in dsa_yuv_pictures.h and its
test (tests/yuv_pictures_test.cpp); these tests check the patch applies them."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


class YuvPicturesPatchTests(unittest.TestCase):
    def setUp(self):
        self.text = read(next(PATCHES.glob("0045-*.patch")))
        lines = self.text.splitlines()
        self.added = "\n".join(
            l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")
        )

    def test_fr26_only_the_frame_buffer_and_the_vulkan_emulation_change(self):
        files = re.findall(r"^diff --git a/(\S+) ", self.text, flags=re.M)
        self.assertEqual(sorted(files), ["host/FrameBuffer.cpp", "host/FrameBuffer.h",
                                         "host/vulkan/VkCommonOperations.cpp",
                                         "host/vulkan/VkCommonOperations.h"])

    def test_fr26_each_color_buffer_records_what_its_uploads_left(self):
        self.assertIn("dsa::YuvPicture picture = dsa::YuvPicture::Unwritten;", self.added)
        self.assertIn("info->picture = dsa::yuvPictureAfterUpload(info->picture, jobs, started, "
                      "finished);", self.added)
        # A job counts as started once submitted, and finished once its fence is seen.
        self.assertIn("vkQueueSubmit(mQueue, 1, &submitInfo, mCommandBufferFence), false);\n"
                      "         }\n"
                      "+        ++uploadEnd.started;", self.text)
        self.assertIn('"Writing a ColorBuffer"), false);\n'
                      "+        ++uploadEnd.finished;", self.text)

    def test_fr26_a_failed_upload_leaves_the_layout_where_the_gpu_left_the_image(self):
        self.assertIn("info->currentLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;\n"
                      "                    info->currentQueueFamilyIndex = family;", self.added)
        self.assertIn("info->currentLayout = layoutBefore;\n"
                      "                    info->currentQueueFamilyIndex = familyBefore;",
                      self.added)

    def test_fr26_a_composition_reading_a_yuv_color_buffer_without_a_picture_is_refused(self):
        self.assertIn("bool VkEmulation::colorBufferAwaitsPicture(", self.added)
        self.assertIn("if (dsa::yuvPictureComposable(colorBufferInfo->picture)) {", self.added)
        # Both request versions, bounded by the buffer the guest sent.
        self.assertIn("(bufferSize - sizeof(ComposeDevice)) / sizeof(ComposeLayer));", self.added)
        self.assertIn("(bufferSize - sizeof(ComposeDevice_v2)) / sizeof(ComposeLayer));",
                      self.added)
        self.assertEqual(self.added.count("return AsyncResult::FAIL_AND_CALLBACK_NOT_SCHEDULED;"),
                         2)

    def test_fr26_only_yuv_color_buffers_are_held(self):
        self.assertIn("if (format != VK_FORMAT_G8_B8R8_2PLANE_420_UNORM &&\n"
                      "        format != VK_FORMAT_G8_B8_R8_3PLANE_420_UNORM) {\n"
                      "        return false;", self.added)

    def test_fr26_planes_that_hold_no_picture_are_refused_before_the_copy(self):
        refused = self.text.index("+    if (dsa::yuvUploadHoldsNoPicture(")
        copied = self.text.index(" return colorBuffer->updateFromBytes(0, 0, "
                                 "static_cast<int>(width)")
        self.assertLess(refused, copied)

    def test_fr26_both_are_counted_in_the_renderer_log(self):
        self.assertIn("dsaYuvPictures().compositionHeld(picture, dsaNowNs(), &line)", self.added)
        self.assertIn("dsaYuvPictures().uploadRefused(dsaNowNs(), &line)", self.added)
        self.assertEqual(self.added.count('WARN("%s (the last: ColorBuffer %u'), 2)

    def test_fr26_no_line_cites_a_planning_document(self):
        self.assertNotRegex(self.added, r"\b(SPEC|INTENT|FR-\d+|NFR-\d+|PR-\d+)\b")

    def test_fr26_every_hunk_line_starts_with_a_diff_marker(self):
        in_hunk = False
        for line in self.text.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-", "\\"), repr(line))

    def test_fr26_the_fetch_applies_0045_after_0044(self):
        self.assertIn(" 0044 0045;", read(HOST / "fetch.sh"))
        self.assertIn('-Filter "0045-*.patch"', read(HOST / "fetch.ps1"))

    def test_fr26_the_rules_have_their_own_test(self):
        self.assertIn('"$here/yuv_pictures_test.cpp"', read(HERE / "tests/test.sh"))


if __name__ == "__main__":
    unittest.main()
