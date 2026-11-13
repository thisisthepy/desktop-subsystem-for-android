"""Tests for releasing an uploaded ColorBuffer to the guest.

What a guest CPU writes into a buffer (a video decoder's pictures, a camera's frames) reaches its
ColorBuffer through updateColorBufferFromBytesLocked, and the guest reads it back through an image
of its own on the same memory, taken from the external queue family. The upload never released
the image there. On the owner's PC (Intel Arc, 0.2.4-dev.12) a YouTube Short, which only the
guest samples, showed rows of green, purple and yellow blocks across the flat top of its picture
while the rest was right, with the host's decoder and the guest's software decoder alike.

Patch 0031 takes the image from the external queue family when it is there and releases it there
once the copy has written, in the command buffer of the last copy."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0031-*.patch"))
NAME = "host/vulkan/VkCommonOperations.cpp"


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


class GuestReleasePatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.removed = sides(PATCH, NAME)

    def test_pr3_only_the_upload_changes(self):
        text = PATCH.read_text(encoding="utf-8")
        files = re.findall(r"^diff --git a/(\S+) ", text, flags=re.M)
        self.assertEqual(files, [NAME])
        hunks = re.findall(r"^@@ .*$", text, flags=re.M)
        self.assertEqual(len(hunks), 3)
        # The read back keeps its own barriers.
        self.assertNotIn("readColorBufferToBytes", self.added + self.removed)

    def test_pr3_the_copy_takes_the_image_from_where_the_guest_left_it(self):
        self.assertIn("const bool takenFromGuest = colorBufferInfo->currentQueueFamilyIndex != "
                      "mQueueFamilyIndex;", self.added)
        self.assertIn(".srcQueueFamilyIndex = takenFromGuest ? "
                      "colorBufferInfo->currentQueueFamilyIndex", self.added)
        self.assertIn(".dstQueueFamilyIndex = takenFromGuest ? mQueueFamilyIndex", self.added)

    def test_pr3_the_written_image_is_released_to_the_guest(self):
        self.assertIn(".srcQueueFamilyIndex = mQueueFamilyIndex,\n"
                      "                .dstQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL,",
                      self.added)
        self.assertIn(".oldLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL,\n"
                      "                .newLayout = guestLayout,", self.added)
        self.assertIn(".srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,", self.added)
        self.assertIn("colorBufferInfo->currentQueueFamilyIndex = VK_QUEUE_FAMILY_EXTERNAL;",
                      self.added)
        self.assertIn("colorBufferInfo->currentLayout = guestLayout;", self.added)

    def test_pr3_an_image_never_used_gets_the_layout_the_guest_is_given(self):
        # As releaseColorBufferForGuestUse() and the compositor leave a ColorBuffer for the
        # guest; the image no longer stays in the transfer layout with the renderer's queue.
        self.assertIn(": VK_IMAGE_LAYOUT_PRESENT_SRC_KHR;", self.added)
        self.assertIn("colorBufferInfo->currentLayout = VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;",
                      self.removed)
        self.assertNotIn("VK_IMAGE_LAYOUT_TRANSFER_DST_OPTIMAL;", self.added)

    def test_pr3_the_release_goes_in_the_last_copys_job(self):
        # No submission of its own: it is recorded where the transition back was.
        self.assertNotIn("vkQueueSubmit", self.added)
        self.assertNotIn("vkBeginCommandBuffer", self.added)
        self.assertEqual(self.added.count("vk->vkCmdPipelineBarrier(mCommandBuffer, "
                                          "VK_PIPELINE_STAGE_TRANSFER_BIT,"), 1)

    def test_pr3_the_fetch_applies_it_after_the_upload_it_changes(self):
        names = sorted(p.name for p in (HERE / "patches").glob("00*.patch"))
        self.assertEqual(names.index(PATCH.name),
                         names.index(next(n for n in names if n.startswith("0030-"))) + 1)
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0031", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
