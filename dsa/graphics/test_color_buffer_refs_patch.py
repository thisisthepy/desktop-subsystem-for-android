"""Tests for the guest Vulkan driver keeping a gralloc buffer's host ColorBuffer while the process
has the buffer.

Without virtio-gpu the host frees a buffer's shared ColorBuffer as soon as no guest process has it
open, and a buffer only the GPU writes holds its pixels nowhere else. The driver closed its
process's reference when the last Vulkan object on the buffer went. hwui uploads a hardware bitmap
through an EGLImage it destroys right after the copy, and the render thread imports the buffer
again when it first draws the bitmap, so a bitmap decoded ahead of drawing came back as a new,
empty ColorBuffer: every thumbnail, ad and avatar in YouTube was an empty grey box.

Now the reference also lasts while the process still has the buffer's descriptors open
(include/dsa_color_buffer_refs.h, tested by tests/color_buffer_refs_test.cpp), and the ones the
process let go of are closed at a submit, at most once a second."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH_DIR = HERE / "patches/guest/mesa"
PATCH = next(PATCH_DIR.glob("0013-*.patch"))
HEADER = HERE / "include/dsa_color_buffer_refs.h"
GRALLOC = "src/gfxstream/guest/android/GrallocMinigbm.cpp"
GRALLOC_HEADER = "src/gfxstream/guest/android/GrallocMinigbm.h"
INTERFACE = "src/gfxstream/guest/android/include/gfxstream/guest/GfxStreamGralloc.h"
TRACKER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class ColorBufferRefsPatchTests(unittest.TestCase):
    def test_pr3_the_last_vulkan_object_on_a_buffer_does_not_close_its_color_buffer(self):
        added, removed = file_hunks(PATCH, GRALLOC)
        text = "\n".join(added)
        # The old rule: the last holder erased the reference and closed it.
        self.assertIn("    mBufferIdToColorBuffer.erase(it);", removed)
        # Now the references decide, with what the process still has open.
        self.assertIn("closed = mColorBufferRefs.drop(bufferId);", text)
        self.assertIn("        if (!mColorBufferRefs.opened(bufferId, hostHandle, handle->data,", text)
        self.assertIn("    mColorBufferRefs.hold(cros_handle->id, hostHandle, handle->data,", text)
        header = HEADER.read_text(encoding="utf-8")
        self.assertIn("if (it->second.holders > 0 || stillOpen(it->second)) return {};", header)

    def test_nfr1_a_released_memory_lets_go_of_its_buffer_before_the_reference_is_dropped(self):
        added, removed = file_hunks(PATCH, GRALLOC)
        # Released first, so a buffer the memory held last closes at once, not a sweep later.
        release = added.index("        AHardwareBuffer_release(ahb);")
        drop = added.index("                closed = mColorBufferRefs.drop(bufferId);")
        self.assertLess(release, drop)
        self.assertIn("        AHardwareBuffer_release(ahb);", removed)

    def test_nfr1_the_references_a_process_let_go_of_close_at_a_submit(self):
        added, _ = file_hunks(PATCH, TRACKER)
        self.assertIn("    if (mGralloc) mGralloc->closeReleasedBuffers();", added)
        added, _ = file_hunks(PATCH, GRALLOC)
        text = "\n".join(added)
        self.assertIn("if (mColorBufferRefs.size() == 0 || !mColorBufferRefs.sweepDue(nowNs)) {", text)
        self.assertIn("closed = mColorBufferRefs.sweep();", text)
        self.assertIn("rcEnc->rcCloseColorBuffer(rcEnc, closed.hostHandle);", text)
        header = HEADER.read_text(encoding="utf-8")
        self.assertIn("static constexpr uint64_t kSweepIntervalNs = 1000000000ULL;", header)

    def test_nfr1_every_gralloc_offers_the_sweep_and_only_minigbm_does_it(self):
        added, _ = file_hunks(PATCH, INTERFACE)
        self.assertIn("    virtual void closeReleasedBuffers() {}", added)
        added, _ = file_hunks(PATCH, GRALLOC_HEADER)
        self.assertIn("    void closeReleasedBuffers() override;", added)
        self.assertIn('#include "dsa_color_buffer_refs.h"', added)
        self.assertIn("    dsa::ColorBufferRefs mColorBufferRefs;", added)

    def test_nfr1_a_buffer_is_kept_by_the_file_its_descriptor_names_not_by_its_number(self):
        header = HEADER.read_text(encoding="utf-8")
        self.assertIn("if (fstat(fd, &st) != 0) return false;", header)
        self.assertIn("if (identify_(file.fd, &device, &inode) && device == file.device &&", header)
        self.assertIn("                inode == file.inode) {", header)

    def test_nfr1_the_patch_comes_after_the_ones_it_builds_on(self):
        names = sorted(p.name for p in PATCH_DIR.glob("*.patch"))
        for before in ("0009-", "0012-"):
            self.assertGreater(names.index(PATCH.name),
                               names.index(next(n for n in names if n.startswith(before))))
        script = (HERE / "fetch.sh").read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', script)
        tests = (HERE / "tests/test.sh").read_text(encoding="utf-8")
        self.assertIn('"$here/color_buffer_refs_test.cpp" -o "$out/color_buffer_refs_test"', tests)
        self.assertIn('"$out/color_buffer_refs_test"', tests)


if __name__ == "__main__":
    unittest.main()
