"""Tests for the guest Vulkan driver's non-coherent cached memory type and its coherent twin.

Without memory the guest and the host share (a Hyper-V VM), guest/mesa 0005 reported the host's
cached host-visible type coherent, which Vulkan requires a type to be, and read the device's
writes back at every point an app learns its work finished. Every app paid that at every frame,
the ones that never need coherence too: ANGLE's upload staging asks for cached memory and
flushes it itself. guest/mesa 0012 reports that type twice (include/dsa_memory_types.h, tested
by tests/memory_types_test.cpp): as the host reports it, non-coherent and read back only when the
app invalidates, first; and as a coherent twin after the host's types, read back at completion.
"""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH_DIR = HERE / "patches/guest/mesa"
PATCH = next(PATCH_DIR.glob("0012-*.patch"))
HEADER = HERE / "include/dsa_memory_types.h"
TRACKER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"
TRACKER_H = "src/gfxstream/guest/vulkan_enc/ResourceTracker.h"
ENCODER = "src/gfxstream/guest/vulkan_enc/VkEncoder.cpp"
TRANSFORM = "src/gfxstream/guest/vulkan_enc/goldfish_vk_transform_guest.cpp"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class NonCoherentMemoryPatchTests(unittest.TestCase):
    def test_pr3_every_way_of_asking_for_the_types_gets_the_split_list(self):
        added, removed = file_hunks(PATCH, TRANSFORM)
        # 0005's in-place coherence is gone; the split replaces it, on the same condition.
        self.assertIn("                flags |= VK_MEMORY_PROPERTY_HOST_COHERENT_BIT;", removed)
        self.assertIn("        resourceTracker->splitMemoryTypes(toTransform);", added)
        added, _ = file_hunks(PATCH, TRACKER)
        text = "\n".join(added)
        self.assertIn("mMemoryTypeSplit.split(properties->memoryTypes, &properties->memoryTypeCount);",
                      text)
        # The header's bits are Vulkan's.
        self.assertIn("dsa::kMemoryHostCoherent == VK_MEMORY_PROPERTY_HOST_COHERENT_BIT", text)
        self.assertIn("dsa::kMaxMemoryTypes == VK_MAX_MEMORY_TYPES", text)
        header, _ = file_hunks(PATCH, TRACKER_H)
        self.assertIn('#include "dsa_memory_types.h"', header)
        self.assertIn("    dsa::MemoryTypeSplit mMemoryTypeSplit;", header)

    def test_pr3_type_indices_and_masks_are_translated_both_ways(self):
        added, removed = file_hunks(PATCH, TRACKER)
        text = "\n".join(added)
        # To the host: an allocation's index (VkMemoryAllocateInfo) and any mask.
        self.assertIn("typeIndex[i] = mMemoryTypeSplit.hostIndex(typeIndex[i]);", text)
        self.assertIn("typeBits[i] = mMemoryTypeSplit.hostBits(typeBits[i]);", text)
        # From the host: every buffer's and image's memoryTypeBits (VkMemoryRequirements, which
        # VkMemoryRequirements2 holds) gains the twin of each cached type in it.
        self.assertIn("typeBits[i] = mMemoryTypeSplit.guestBits(typeBits[i]);", text)
        self.assertEqual(removed.count("    (void)typeBits;"), 2)
        # The translation runs before the loop over allocations, which returns early for one
        # it does not know.
        raw = PATCH.read_text(encoding="utf-8")
        self.assertLess(raw.index("+            typeIndex[i] = mMemoryTypeSplit.hostIndex(typeIndex[i]);"),
                        raw.index("     if (memory) {\n         std::lock_guard<std::recursive_mutex> lock(mLock);"))

    def test_pr3_a_color_buffer_never_takes_a_twin_the_host_does_not_know(self):
        added, _ = file_hunks(PATCH, TRACKER)
        text = "\n".join(added)
        # AHardwareBuffer and sync-fd memory properties name the color buffer's type alone.
        self.assertIn("if (isAddedMemoryType(static_cast<uint32_t>(i))) continue;", text)
        self.assertIn("return mMemoryTypeSplit.isTwin(typeIndex);", text)

    def test_fr4_only_a_coherent_type_is_read_back_when_the_apps_work_finishes(self):
        added, removed = file_hunks(PATCH, TRACKER)
        # Every completion point (vkWaitForFences, vkGetFenceStatus, vkQueueWaitIdle,
        # vkDeviceWaitIdle, the semaphore waits) goes through invalidateShadowedMemory.
        self.assertIn("                  VK_MEMORY_PROPERTY_HOST_CACHED_BIT)) {", removed)
        text = "\n".join(added)
        self.assertIn("!dsa::readBackAtCompletion(", text)
        self.assertIn("memProps.memoryTypes[info.memoryTypeIndex].propertyFlags)) {", text)
        header = HEADER.read_text(encoding="utf-8")
        self.assertIn("return (guestFlags & kMemoryHostVisible) && (guestFlags & kMemoryHostCoherent);",
                      header)

    def test_fr4_the_apps_own_invalidate_still_reads_back_every_type(self):
        # A non-coherent type's writes come back through vkInvalidateMappedMemoryRanges, which
        # the encoder reads for every mapped allocation, with no memory type check.
        encoder = PATCH_DIR / "0010-read-back-only-what-the-device-changed.patch"
        added, _ = file_hunks(encoder, ENCODER)
        self.assertIn("                    sResourceTracker->receiveReadBack(memory, at, piece.data(), length);",
                      added)
        added, _ = file_hunks(PATCH, ENCODER)
        self.assertNotIn("readBackAtCompletion", "\n".join(added))

    def test_pr3_whole_size_ranges_stay_inside_the_allocation(self):
        added, removed = file_hunks(PATCH, ENCODER)
        text = "\n".join(added)
        # The flush sent the mapping's size from the range's offset: past the copy's end.
        self.assertIn("                size == VK_WHOLE_SIZE ? sResourceTracker->getMappedSize(memory) : size;",
                      removed)
        self.assertIn("const uint64_t left = offset < mappedSize ? mappedSize - offset : 0;", text)
        self.assertIn("size == VK_WHOLE_SIZE ? left : std::min<uint64_t>(size, left);", text)
        # The host is told the range in bytes, so it neither reads nor sends past its mapping.
        added, _ = file_hunks(PATCH, TRACKER)
        text = "\n".join(added)
        self.assertIn("size[i] = offset[i] < end ? end - offset[i] : 0;", text)
        self.assertIn("const uint64_t end = info.shadowSize;", text)

    def test_pr3_it_applies_after_the_patches_it_builds_on_and_its_tests_run(self):
        names = sorted(p.name for p in PATCH_DIR.glob("*.patch"))
        # After 0005 (the coherent type), 0007 (shadowSize, the first map) and 0010 (pieces).
        for before in ("0005-", "0007-", "0010-"):
            self.assertGreater(names.index(PATCH.name),
                               names.index(next(n for n in names if n.startswith(before))))
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do',
                      (HERE / "fetch.sh").read_text(encoding="utf-8"))
        tests = (HERE / "tests/test.sh").read_text(encoding="utf-8")
        self.assertIn('"$here/memory_types_test.cpp"', tests)
        gate = (HERE.parent.parent / "scripts/tests/graphics-source.test.sh").read_text(encoding="utf-8")
        self.assertIn("uv run dsa/graphics/test_noncoherent_memory_patch.py", gate)


if __name__ == "__main__":
    unittest.main()
