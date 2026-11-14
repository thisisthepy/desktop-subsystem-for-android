"""Tests for the guest Vulkan driver closing a window buffer's host ColorBuffer with its image.

Every process that uses a gralloc buffer opens the buffer's host ColorBuffer once and keeps
the reference until it lets go of the buffer. The driver let go only of buffers imported into
device memory. A window's buffers reach it differently: Android's swapchain makes a VkImage on
each one (VkNativeBufferANDROID), and destroying the image closed nothing. So every buffer an
app's window ever had stayed on the host until the app's process ended: each resize, each time
the window was hidden and shown again. With one Play Store window open for an hour the
renderer's GPU allocations grew from 148 MiB in 11 to 475 MiB in 47, on an integrated GPU,
where that memory is the host's RAM.

Now each image on a window buffer, and each device memory that imported a buffer, holds the
process's reference, and the last of them to go closes it."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH_DIR = HERE / "patches/guest/mesa"
PATCH = next(PATCH_DIR.glob("0009-*.patch"))
GRALLOC = "src/gfxstream/guest/android/GrallocMinigbm.cpp"
GRALLOC_HEADER = "src/gfxstream/guest/android/GrallocMinigbm.h"
INTERFACE = "src/gfxstream/guest/android/include/gfxstream/guest/GfxStreamGralloc.h"
TRACKER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"
TRACKER_HEADER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.h"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class WindowBufferPatchTests(unittest.TestCase):
    def test_nfr1_destroying_a_window_image_drops_its_color_buffer(self):
        added, _ = file_hunks(PATCH, TRACKER)
        text = "\n".join(added)
        # Made on a window buffer, or bound to one: the image holds the buffer's reference.
        self.assertIn("holdWindowBuffer(info, (const native_handle_t*)anbInfoPtr->handle);", text)
        self.assertIn("holdWindowBuffer(imageIt->second, (const native_handle_t*)anb->handle);",
                      text)
        # Its destruction lets go of it.
        self.assertIn("mGralloc->dropHostHandle(it->second.windowBufferHostHandle);", text)
        added, _ = file_hunks(PATCH, TRACKER_HEADER)
        self.assertIn("        uint32_t windowBufferHostHandle = 0;", added)

    def test_nfr1_imported_memory_and_window_images_share_one_counted_reference(self):
        added, _ = file_hunks(PATCH, TRACKER)
        self.assertIn(
            "        const uint32_t hostHandle = mGralloc->holdHostHandle(mGralloc->getNativeHandle(ahw));",
            added)
        added, removed = file_hunks(PATCH, GRALLOC)
        text = "\n".join(added)
        # The memory's release is one holder going, not the reference closing outright, so a
        # window image on the same buffer keeps it.
        self.assertIn("                    mBufferIdToColorBuffer.erase(it);", removed)
        self.assertIn("hostHandle = dropLocked(cros_handle->id);", text)
        self.assertIn("    if (it->second.holders > 0) {\n        it->second.holders--;\n    }", text)
        # The last holder closes it with the host.
        self.assertIn("rcEnc->rcCloseColorBuffer(rcEnc, hostHandle);", text)
        added, _ = file_hunks(PATCH, GRALLOC_HEADER)
        self.assertIn("    std::unordered_map<uint32_t, OpenColorBuffer> mBufferIdToColorBuffer;", added)

    def test_nfr1_a_second_open_of_one_buffer_is_closed_again(self):
        # Two threads opening the same buffer at once keep one reference, not two.
        added, _ = file_hunks(PATCH, GRALLOC)
        self.assertIn(
            "        if (!mBufferIdToColorBuffer.emplace(bufferId, OpenColorBuffer{hostHandle, 0}).second) {",
            added)

    def test_nfr1_every_gralloc_offers_the_hold_and_only_minigbm_counts_it(self):
        added, _ = file_hunks(PATCH, INTERFACE)
        text = "\n".join(added)
        self.assertIn("    virtual uint32_t holdHostHandle(const native_handle_t* handle) {\n"
                      "        return getHostHandle(handle);\n    }", text)
        self.assertIn("    virtual void dropHostHandle(uint32_t /*hostHandle*/) {}", text)
        added, _ = file_hunks(PATCH, GRALLOC_HEADER)
        self.assertIn("    uint32_t holdHostHandle(const native_handle_t* handle) override;", added)
        self.assertIn("    void dropHostHandle(uint32_t hostHandle) override;", added)

    def test_nfr1_the_patch_comes_after_the_ones_it_builds_on(self):
        names = sorted(p.name for p in PATCH_DIR.glob("*.patch"))
        # After every patch numbered before it; later ones (0010 reads memory back in
        # pieces) build on other code.
        self.assertGreater(names.index(PATCH.name),
                           names.index(next(n for n in names if n.startswith("0008-"))))
        script = (HERE / "fetch.sh").read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', script)


if __name__ == "__main__":
    unittest.main()
