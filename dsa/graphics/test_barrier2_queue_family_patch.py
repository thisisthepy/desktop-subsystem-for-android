"""Tests for converting a synchronization2 barrier's foreign queue family.

The decoder replaces VK_QUEUE_FAMILY_FOREIGN_EXT in a guest's barriers with
VK_QUEUE_FAMILY_EXTERNAL and records which ColorBuffers a command buffer takes and gives back.
For vkCmdPipelineBarrier2 it read each barrier as the first version's struct, whose queue family
fields sit where VkImageMemoryBarrier2 keeps its 64-bit stage masks. Patch 0033 converts each
barrier through its own type."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0033-*.patch"))
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


class Barrier2QueueFamilyPatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.removed = sides(PATCH, DECODER)

    def test_pr3_only_the_decoder_changes(self):
        text = PATCH.read_text(encoding="utf-8")
        files = re.findall(r"^diff --git a/(\S+) ", text, flags=re.M)
        self.assertEqual(files, [DECODER])

    def test_pr3_the_second_version_barriers_are_read_as_their_own_type(self):
        self.assertIn("((VkImageMemoryBarrier2*)pDependencyInfo->pImageMemoryBarriers) + i);",
                      self.added)
        self.assertIn("((VkBufferMemoryBarrier2*)pDependencyInfo->pBufferMemoryBarriers) + i);",
                      self.added)
        self.assertIn("((VkImageMemoryBarrier*)pDependencyInfo->pImageMemoryBarriers) + i);",
                      self.removed)
        self.assertIn("((VkBufferMemoryBarrier*)pDependencyInfo->pBufferMemoryBarriers) + i);",
                      self.removed)
        self.assertNotIn("(VkImageMemoryBarrier*)pDependencyInfo", self.added)

    def test_pr3_both_queue_family_fields_are_converted(self):
        for kind in ("VkBufferMemoryBarrier2", "VkImageMemoryBarrier2"):
            body = self.added[self.added.index(f"convertQueueFamilyForeignToExternal_{kind}(\n"
                                               f"        {kind}* barrier) {{"):]
            body = body[:body.index("    }")]
            self.assertIn("convertQueueFamilyForeignToExternal(&barrier->srcQueueFamilyIndex);",
                          body)
            self.assertIn("convertQueueFamilyForeignToExternal(&barrier->dstQueueFamilyIndex);",
                          body)

    def test_pr3_the_fetch_applies_it_after_0032(self):
        names = sorted(p.name for p in (HERE / "patches").glob("00*.patch"))
        self.assertEqual(names.index(PATCH.name),
                         names.index(next(n for n in names if n.startswith("0032-"))) + 1)
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0033", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
