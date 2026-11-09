"""Tests for the guest Vulkan driver's copy of what the GPU drew back into a gralloc buffer's
memory, for a CPU that reads it. A Hyper-V VM has no virtio-gpu device, so the drawing is only
in the host ColorBuffer: screencap, an IDE's screenshot and an ImageReader read zeros without
it. The policy itself is in include/dsa_cpu_readback.h, tested by tests/cpu_readback_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches/guest/mesa").glob("0002-*.patch"))
FETCH = HERE / "fetch.sh"
HEADER = HERE / "include/dsa_cpu_readback.h"
TESTS = HERE / "tests/test.sh"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    parts = re.split(r"^diff --git ", text, flags=re.M)
    for part in parts:
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


def function(added, signature):
    """The added lines of one function, from its signature to its closing brace."""
    start = next(i for i, line in enumerate(added) if line.startswith(signature))
    end = next(i for i in range(start, len(added)) if added[i] == "}")
    return "\n".join(added[start:end + 1])


class CpuReadbackPatchTests(unittest.TestCase):
    def test_pr3_the_minigbm_gralloc_reads_the_colorbuffer_into_the_buffers_memory(self):
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/android/GrallocMinigbm.cpp")
        wants = function(added, "bool MinigbmGralloc::wantsCpuReadback(")
        # Only without virtio-gpu, whose kernel moves the drawing itself.
        self.assertIn("if (mDevice || !handle) {", wants)
        self.assertIn("dsa::wantsCpuReadback(static_cast<uint64_t>(cros_handle->usage))", wants)
        self.assertIn("dsa::readbackFormat(cros_handle->format, &format)", wants)

        read = function(added, "int MinigbmGralloc::readBackForCpu(")
        self.assertIn("const uint32_t hostHandle = getHostHandle(handle);", read)
        self.assertIn("rcEnc->rcReadColorBuffer(rcEnc, hostHandle, 0, 0,", read)
        self.assertIn("format.glFormat, format.glType", read)
        # Into the memory a CPU lock maps, at the buffer's offset and stride.
        self.assertIn("mmap(nullptr, mapSize, PROT_READ | PROT_WRITE, MAP_SHARED, fd, 0)", read)
        self.assertIn("const size_t stride = cros_handle->strides[0];", read)
        self.assertIn("dsa::copyReadbackRows(", read)
        self.assertIn("DMA_BUF_IOCTL_SYNC", read)
        self.assertIn("munmap(mapped, mapSize);", read)
        self.assertIn('#include "dsa_cpu_readback.h"', added)

    def test_pr3_every_gralloc_offers_the_read_back_and_only_minigbm_does_it(self):
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/android/include/gfxstream/guest/GfxStreamGralloc.h")
        self.assertIn("    virtual bool wantsCpuReadback(const native_handle_t* /*handle*/) { return false; }",
                      added)
        self.assertIn("    virtual int readBackForCpu(const native_handle_t* /*handle*/) { return -1; }", added)
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/android/GrallocMinigbm.h")
        self.assertIn("    bool wantsCpuReadback(const native_handle_t* handle) override;", added)
        self.assertIn("    int readBackForCpu(const native_handle_t* handle) override;", added)

    def test_pr3_imported_buffers_a_cpu_reads_are_tracked_from_import_to_free(self):
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp")
        text = "\n".join(added)
        self.assertIn("mGralloc->wantsCpuReadback(mGralloc->getNativeHandle(info.ahw))", text)
        self.assertIn("mCpuReadback.add((uint64_t)memory, (uint64_t)device, info.ahw);", text)
        self.assertIn("    mCpuReadback.remove((uint64_t)mem);", added)
        # Window images (an ImageReader's buffers), made on or bound to a gralloc buffer.
        self.assertIn("mGralloc->wantsCpuReadback((const native_handle_t*)anbInfoPtr->handle)", text)
        self.assertIn("mGralloc->wantsCpuReadback((const native_handle_t*)anb->handle)", text)

    def test_pr3_the_drawing_comes_back_wherever_the_cpu_learns_it_is_done(self):
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp")
        # Fences and semaphores handed out (SurfaceFlinger's capture), and the driver's waits.
        self.assertEqual(added.count("        readBackForCpu(enc, device);"), 2)
        self.assertIn("            readBackForCpu(vkEncoder, device);", added)
        self.assertIn("        if (device != VK_NULL_HANDLE) readBackForCpu(enc, device);", added)
        # A window image released to its consumer.
        self.assertIn("        if (result == VK_SUCCESS) readBackImageForCpu(image);", added)
        # Each waits until the host has the drawing.
        read = function(added, "void ResourceTracker::readBackForCpu(")
        self.assertIn("enc->vkDeviceWaitIdle(device, true /* do lock */) == VK_SUCCESS", read)
        self.assertIn("AHardwareBuffer_acquire(ahb);", read)
        self.assertIn("AHardwareBuffer_release(ahb);", read)

    def test_pr3_composition_without_cpu_readers_pays_nothing(self):
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp")
        text = "\n".join(added)
        self.assertIn("    if (!mCpuReadback.empty()) {", added)
        self.assertIn("mCpuReadback.submitted((uint64_t)queueIt->second.device);", text)
        read = function(added, "void ResourceTracker::readBackForCpu(")
        self.assertLess(read.index("if (mCpuReadback.empty()) return;"),
                        read.index("vkDeviceWaitIdle"))
        self.assertLess(read.index("if (buffers.empty()) return;"), read.index("vkDeviceWaitIdle"))
        added, _ = file_hunks(PATCH, "src/gfxstream/guest/vulkan_enc/ResourceTracker.h")
        self.assertIn('#include "dsa_cpu_readback.h"', added)
        self.assertIn("    dsa::CpuReadbackSet mCpuReadback;", added)
        self.assertIn("class CpuReadbackSet {", HEADER.read_text(encoding="utf-8"))

    def test_pr3_fetch_applies_it_after_the_patches_it_builds_on_and_its_test_runs(self):
        script = FETCH.read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', script)
        self.assertLess(script.index('"$patch_dir"/0008-*.patch'), script.index('"$patch_dir"/guest/mesa/*.patch'))
        names = sorted(p.name for p in (HERE / "patches/guest/mesa").glob("*.patch"))
        # After the lost-stream patch, whose tree it is made on.
        self.assertEqual(names[names.index(PATCH.name) - 1][:5], "0001-")
        self.assertIn('"$here/cpu_readback_test.cpp"', TESTS.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
