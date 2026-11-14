"""Tests for making the renderer's lost Vulkan device again without making the guest wait.

On the owner's PC (Intel Arc, Windows 11, v0.2.3, 2026-11-12) a GPU reset lost the renderer's
device at 00:50:38.470 UTC, and the renderer logged a new one at 00:50:51.875: 13.4 s, all of
it under the emulation's lock and its device lock. Every guest call that touched a
ColorBuffer waited; the guest's own lost devices were reported only after it (00:50:52.894),
YouTube's input went unanswered, and Android ended it. A memory report 6.3 s into the remake
counted no ColorBuffer left and none made yet, and the window manager could make its own
Direct3D device only 26 ms after the renderer's: the GPU was not back from its reset.

Patch 0021 makes the device again on a thread of its own (dsa_device_remake.h): what needs it
fails at once meanwhile, the slow steps run without the lock, and each ColorBuffer is made
again under the lock on its own. Every wait for the GPU under the lock is bounded, and the
suspect check no longer waits for the queue to go idle. The C++ half is tested in
tests/device_remake_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0021-*.patch"))
SOURCE = "host/vulkan/VkCommonOperations.cpp"
HEADER = "host/vulkan/VkCommonOperations.h"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


def new_side(patch, name):
    """The lines of one file as the patch leaves them, hunk after hunk (added and context)."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return [l[1:] for l in part.splitlines()
                    if (l.startswith("+") and not l.startswith("+++")) or l.startswith(" ")]
    raise AssertionError(f"{patch.name} does not change {name}")


def function(lines, signature):
    """The lines of the function that starts with `signature`, as the patch leaves it."""
    start = next(i for i, l in enumerate(lines) if l.startswith(signature))
    end = next(i for i in range(start + 1, len(lines)) if lines[i] == "}")
    return "\n".join(lines[start:end + 1])


class DeviceRemakePatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.removed = file_hunks(PATCH, SOURCE)
        self.text = "\n".join(self.added)

    def test_nfr5_a_lost_device_is_made_again_on_a_thread_of_its_own(self):
        recover = function(self.after, "bool VkEmulation::recoverIfLost()")
        # Nothing waits for the device being made, and recoverIfLost never makes it itself.
        self.assertIn("if (mRemake.remaking()) {\n        return false;", recover)
        self.assertIn("startRemake();\n    return false;", recover)
        self.assertNotIn("remakeDevice", recover)
        self.assertNotIn("unique_lock", recover)
        start = function(self.after, "void VkEmulation::startRemake()")
        self.assertIn("mRemake.claim()", start)
        self.assertIn("std::thread([this] { remakeDevice(); })", start)
        # The old remake ran under both locks on the caller's thread.
        self.assertIn("bool VkEmulation::remakeDeviceLocked() {", self.removed)

    def test_nfr5_the_slow_steps_run_without_the_lock(self):
        remake = function(self.after, "void VkEmulation::remakeDevice()")
        # The lost device is destroyed and the new one made outside every lock scope.
        destroy = remake.index("mIvk->vkDestroyDevice(lost, nullptr);")
        make = remake.index("const VkResult made = mMakeDevice(&device);")
        release = remake.index("lost = releaseLostDeviceLocked(&hadCompositor);")
        install = remake.index("mDevice = device;")
        self.assertLess(release, destroy)
        self.assertLess(make, install)
        between = remake[release:install]
        # One lock scope closes before the destroy, the next opens after the make.
        self.assertEqual(between.count("std::lock_guard<std::mutex> lock(mMutex);"), 1)
        # A device that cannot be made yet is tried again soon, not at the next operation.
        self.assertIn("std::this_thread::sleep_for(dsa::remake_retry_delay(failures++));", remake)
        # Each ColorBuffer under the lock on its own.
        loop = remake[remake.index("One ColorBuffer at a time"):]
        self.assertIn("std::lock_guard<std::mutex> lock(mMutex);\n            if (mLostColorBuffers.empty()", loop)
        self.assertIn("mRemake.finished(true);", remake)
        # How long each step took, for the next report.
        self.assertIn("objects were freed after %lld ms and it was destroyed after %lld ms", remake)

    def test_nfr5_a_colorbuffer_the_guest_destroys_meanwhile_is_not_made_again(self):
        self.assertIn("    if (mLostColorBuffers.erase(colorBufferHandle) > 0) {", self.added)

    def test_nfr5_no_wait_for_the_gpu_under_the_lock_is_unbounded(self):
        # The suspect check waited for the queue to go idle, and readbacks 5 s and 10 s more.
        self.assertIn("        result = mDvk->vkQueueWaitIdle(mQueue);", self.removed)
        self.assertNotIn("ANB_MAX_WAIT_NS", self.text)
        self.assertNotIn("vkQueueWaitIdle(mQueue)", function(self.after, "bool VkEmulation::deviceLostLocked()"))
        self.assertIn("dsa::kSuspectProbeNs", self.text)
        self.assertEqual(self.text.count("waitForFenceBounded(vk, "), 5)
        wait = function(self.after, "VkResult VkEmulation::waitForFenceBounded(")
        self.assertIn("dsa::wait_bounded(", wait)
        self.assertIn("[this] { return mRemake.isLost(); }", wait)

    def test_nfr5_the_header_counts_losses_and_the_end_waits_for_the_thread(self):
        added, _ = file_hunks(PATCH, HEADER)
        header = "\n".join(added)
        self.assertIn('#include "dsa_device_remake.h"', header)
        self.assertIn("uint64_t deviceLosses() const { return mRemake.losses(); }", header)
        self.assertIn("bool deviceUsable() const { return mRemake.usable(); }", header)
        self.assertIn("mEnding.store(true);", self.text)
        self.assertIn("            mRemaker.join();", self.added)

    def test_nfr5_the_fetch_applies_it_and_the_backend_reports_losses(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0021", script.read_text(encoding="utf-8"))
        cmake = (HERE / "host/CMakeLists.txt").read_text(encoding="utf-8")
        self.assertIn("target_include_directories(gfxstream-vulkan-server PUBLIC", cmake)
        backend = (HERE / "host/dsa_gfx_backend.cpp").read_text(encoding="utf-8")
        self.assertIn("uint64_t dsa_gfx_device_losses(void)", backend)
        tests = (HERE / "tests/test.sh").read_text(encoding="utf-8")
        self.assertIn('"$here/device_remake_test.cpp"', tests)


class BoundedCopiesPatchTests(unittest.TestCase):
    """Patch 0022: a ColorBuffer read back or written whole goes in jobs of at most
    dsa::kMaxCopyPixels, each submitted and waited for on its own, so none grows with a
    window maximized on a 200% monitor (2880x2048 and more)."""

    def setUp(self):
        self.patch = next((HERE / "patches").glob("0022-*.patch"))
        self.after = new_side(self.patch, SOURCE)
        self.added, _ = file_hunks(self.patch, SOURCE)

    def test_nfr5_a_whole_colorbuffer_copy_is_cut_into_bands_of_rows(self):
        cut = function(self.after, "std::vector<std::vector<VkBufferImageCopy>> boundedCopies(")
        self.assertIn("dsa::copy_bands(whole.imageExtent.width, height)", cut)
        self.assertIn("part.bufferOffset = rowBytes * band.y;", cut)
        self.assertIn("part.imageExtent.height = band.rows;", cut)
        self.assertIn('#include "dsa_copy_bands.h"', self.added)

    def test_nfr5_each_band_is_its_own_submission_and_wait(self):
        text = "\n".join(self.after)
        for label, copy in (('"readColorBufferToBytes(ColorBuffer:%d)"', "vkCmdCopyImageToBuffer"),
                            ('"updateColorBufferFromBytes(ColorBuffer:%d)"',
                             "vkCmdCopyBufferToImage")):
            at = text.index(label)
            start = text.rindex("for (size_t group = 0; group < groups.size(); ++group) {", 0, at)
            loop = text[start:text.index("VK_EMU_TRY(vk->vkResetFences(", at)]
            self.assertLess(loop.index(copy), loop.index("vkQueueSubmit"))
            self.assertLess(loop.index("vkQueueSubmit"), loop.index("waitForFenceBounded"))
            self.assertIn("cmdAfterEarlierCopies(vk, mCommandBuffer);", loop)

    def test_nfr5_a_buffer_copy_goes_in_chunks_each_its_own_submission(self):
        text = "\n".join(self.added)
        self.assertEqual(text.count("    for (uint64_t done = 0; done < size;) {"), 2)
        self.assertEqual(
            text.count("const uint64_t chunk = std::min<uint64_t>(size - done, dsa::kMaxCopyBytes);"),
            2)
        self.assertIn("            .size = chunk,", self.added)

    def test_nfr5_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0022", script.read_text(encoding="utf-8"))


class ColorBufferDuringRemakePatchTests(unittest.TestCase):
    """Patch 0029: a ColorBuffer asked for while the lost device is made again fails that call.

    On the Linux renderer test (llvmpipe, 2026-11-12) a guest's rcCreateColorBuffer right after
    a simulated loss found the device lost, ColorBufferVk could not be made, and FrameBuffer
    aborted ("FATAL in createColorBufferWithResourceHandleLocked"), which ended the guest's
    stream 15 ms before the new device was made."""

    def setUp(self):
        self.patch = next((HERE / "patches").glob("0029-*.patch"))
        self.added, self.removed = file_hunks(self.patch, "host/FrameBuffer.cpp")
        self.after = new_side(self.patch, "host/FrameBuffer.cpp")

    def test_nfr5_a_color_buffer_asked_for_during_the_remake_fails_without_ending_the_stream(self):
        text = "\n".join(self.after)
        before = text.index("lossesBefore = m_emulationVk ? m_emulationVk->deviceLosses() : 0;")
        create = text.index("ColorBuffer::create(")
        lost = text.index("!m_emulationVk->deviceUsable() || "
                          "m_emulationVk->deviceLosses() != lossesBefore")
        fails = text.index("return 0;", lost)
        fatal = text.index("GFXSTREAM_ABORT(FatalError(ABORT_REASON_OTHER))")
        # The losses are counted before the creation, so a loss found by the creation itself
        # counts, even when the new device is made before the check.
        self.assertLess(before, create)
        self.assertLess(create, lost)
        # A device loss answers the call before the fatal path; other failures stay fatal.
        self.assertLess(fails, fatal)
        self.assertEqual(self.removed, [])

    def test_nfr5_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0029", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
