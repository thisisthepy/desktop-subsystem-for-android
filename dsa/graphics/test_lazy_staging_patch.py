"""Tests for making the renderer's staging buffer only when a copy needs it.

The renderer made a 128 MiB host-visible staging buffer at the start (kDefaultStagingBufferSize)
and again with every device made after a GPU reset, mapped it, and kept it for its whole life.
It is used only when a guest CPU reads or writes a color buffer or a buffer. On an integrated
GPU (the owner's Intel Arc) host-visible memory is system memory: the renderer's private bytes
read 195 to 201 MiB with no guest connected, and the buffer was most of it.

Patch 0026 makes the buffer in ensureStagingLocked() when a copy needs it, as large as the copy
(rounded up to 4 MiB, never more than the old size), and releaseIdleStaging() frees it once no
copy used it for a while; the renderer's trim thread (dsa_gfx_memory.cpp) calls it every
second with five seconds of idle. The run on a GPU (software Vulkan in CI) is
nfr1_the_staging_buffer_is_made_for_a_copy_and_freed_when_idle in
dsa/host/platform/windows/tests/window_manager.rs."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0026-*.patch"))
MEMORY = HERE / "host" / "dsa_gfx_memory.cpp"


def sides(patch, name):
    """The added lines and the lines the patch leaves (added and context) of one file."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
            after = [l[1:] for l in lines
                     if (l.startswith("+") and not l.startswith("+++")) or l.startswith(" ")]
            removed = [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")]
            return "\n".join(added), "\n".join(after), "\n".join(removed)
    raise AssertionError(f"{patch.name} does not change {name}")


class LazyStagingPatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.after, self.removed = sides(PATCH, "host/vulkan/VkCommonOperations.cpp")

    def test_nfr1_no_staging_buffer_is_made_at_the_start_or_with_a_new_device(self):
        # Both places that made one of mStaging.size no longer do.
        self.assertIn("emulation->mStaging.size,", self.removed)
        self.assertIn("bufCi.size = mStaging.size;", self.removed)
        self.assertNotIn("bufCi.size = mStaging.size;", self.added)
        self.assertIn("The staging buffer is made when a copy first needs it", self.added)

    def test_nfr1_every_copy_makes_the_staging_buffer_it_needs(self):
        self.assertEqual(self.added.count("if (!ensureStagingLocked("), 4)
        for size in ("bufferCopySize", "dstBufferSize", "size"):
            self.assertIn(f"if (!ensureStagingLocked({size})) {{", self.added)

    def test_nfr1_the_staging_buffer_is_as_large_as_the_copy(self):
        ensure = self.added[self.added.index("bool VkEmulation::ensureStagingLocked("):]
        ensure = ensure[:ensure.index("\n}\n")]
        self.assertIn("mStaging.allocated >= bytes", ensure)
        self.assertIn("if (bytes > mStaging.size) {", ensure)
        self.assertIn("(bytes + kStep - 1) / kStep * kStep", ensure)
        self.assertIn("bufCi.size = want;", ensure)
        self.assertIn("mStaging.lastUse = std::chrono::steady_clock::now();", ensure)

    def test_nfr1_an_idle_staging_buffer_is_freed(self):
        idle = self.added[self.added.index("void VkEmulation::releaseIdleStaging("):]
        idle = idle[:idle.index("\n}\n")]
        self.assertIn("std::lock_guard<std::mutex> lock(mMutex);", idle)
        self.assertIn("std::chrono::steady_clock::now() - mStaging.lastUse < idle", idle)
        self.assertIn("releaseStagingLocked();", idle)
        source = MEMORY.read_text(encoding="utf-8")
        run = source[source.index("void trim_run() {"):]
        self.assertIn("emu->releaseIdleStaging(kStagingIdle);", run[:run.index("\n}\n")])
        self.assertIn("constexpr std::chrono::seconds kStagingIdle{5};", source)

    def test_nfr1_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0026", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
