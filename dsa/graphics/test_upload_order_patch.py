"""Tests for ordering a ColorBuffer's upload after the GPU work around it.

What a guest CPU writes into a buffer (a video decoder's pictures, a camera's frames) reaches the
buffer's ColorBuffer through updateColorBufferFromBytesLocked: a transition of the image to the
transfer layout, the copies of its planes (each plane in a job of its own since 0022), and a
transition back. The first transition went from the top of the pipe to the host stage and the
last from the host stage to the bottom of the pipe. A copy runs in the transfer stage, which
neither scope held, so a transition could run beside a copy, and nothing ordered the copy after
an earlier frame still sampling the image. On the owner's PC (Intel Arc, 0.2.4-dev.9) a YouTube
Short decoded on the host showed blocks of green and purple along the top rows of its picture,
the rows copied first, and flickered green.

Patch 0030 orders the first transition after all earlier commands on the queue and before the
copy, and the last after the copy and before every later use."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0030-*.patch"))
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


class UploadOrderPatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.removed = sides(PATCH, NAME)

    def test_fr26_the_copy_waits_for_the_transition_and_earlier_work(self):
        self.assertIn("VK_PIPELINE_STAGE_TOP_OF_PIPE_BIT,\n"
                      "                                     VK_PIPELINE_STAGE_HOST_BIT",
                      self.removed)
        self.assertIn("vk->vkCmdPipelineBarrier(mCommandBuffer, VK_PIPELINE_STAGE_ALL_COMMANDS_BIT,\n"
                      "                                     VK_PIPELINE_STAGE_TRANSFER_BIT",
                      self.added)
        self.assertIn(".srcAccessMask = VK_ACCESS_MEMORY_READ_BIT | VK_ACCESS_MEMORY_WRITE_BIT,",
                      self.added)
        self.assertIn(".dstAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,", self.added)

    def test_fr26_later_uses_wait_for_the_copy(self):
        self.assertIn("vk->vkCmdPipelineBarrier(mCommandBuffer, VK_PIPELINE_STAGE_HOST_BIT,\n"
                      "                                         VK_PIPELINE_STAGE_BOTTOM_OF_PIPE_BIT",
                      self.removed)
        self.assertEqual(self.added.count("vk->vkCmdPipelineBarrier(mCommandBuffer, "
                                          "VK_PIPELINE_STAGE_TRANSFER_BIT,\n"
                                          "                                         "
                                          "VK_PIPELINE_STAGE_ALL_COMMANDS_BIT"), 2)
        self.assertEqual(self.added.count(".srcAccessMask = VK_ACCESS_TRANSFER_WRITE_BIT,"), 2)
        # An image left in the transfer layout still has the copy made visible.
        self.assertIn("const VkMemoryBarrier written = {", self.added)
        self.assertIn("0, 1, &written, 0,", self.added)

    def test_fr26_only_the_upload_changes(self):
        # The read back keeps its own barriers; only the upload's change.
        self.assertNotIn("readColorBufferToBytes", self.added + self.removed)

    def test_fr26_the_fetch_applies_it_after_the_copies_it_orders(self):
        names = sorted(p.name for p in (HERE / "patches").glob("00*.patch"))
        self.assertGreater(names.index(PATCH.name),
                           names.index(next(n for n in names if n.startswith("0022-"))))
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0030", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
