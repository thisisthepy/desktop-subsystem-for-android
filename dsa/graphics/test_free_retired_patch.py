"""Tests for freeing a destroyed ColorBuffer's Vulkan objects once the GPU is done with them.

Patch 0009 stopped a destroyed ColorBuffer from waiting, under the renderer's locks, for the
queue to go idle: its image and memory are retired behind a fence and freed once the fence
signals. But retired objects were looked at only when the next ColorBuffer or Buffer was made
or destroyed, and the fence of the one just destroyed has not signaled by then. When a window
closed, its buffers (each as large as the window: 33 MiB at 2880x2880) stayed allocated until
another window opened, which on the owner's PC (Intel Arc, where GPU memory is the process's
private memory) kept the renderer's private bytes from falling when windows closed.

Patch 0025 gives the emulation freeFinishedRetired(), and the renderer's trim thread
(dsa_gfx_memory.cpp) calls it once a second. The run on a GPU (software Vulkan in CI) is
nfr2_a_destroyed_color_buffer_is_freed_without_another_call in
dsa/host/platform/windows/tests/window_manager.rs."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0025-*.patch"))
MEMORY = HERE / "host" / "dsa_gfx_memory.cpp"


def added(patch, name):
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return "\n".join(l[1:] for l in part.splitlines()
                             if l.startswith("+") and not l.startswith("+++"))
    raise AssertionError(f"{patch.name} does not change {name}")


class FreeRetiredPatchTests(unittest.TestCase):
    def test_nfr2_the_emulation_frees_finished_retired_objects_on_request(self):
        header = added(PATCH, "host/vulkan/VkCommonOperations.h")
        self.assertIn("void freeFinishedRetired();", header)
        source = added(PATCH, "host/vulkan/VkCommonOperations.cpp")
        body = source[source.index("void VkEmulation::freeFinishedRetired() {"):]
        self.assertIn("std::lock_guard<std::mutex> lock(mMutex);", body)
        # Without waiting: only what the GPU finished goes.
        self.assertIn("freeRetiredLocked(/*wait=*/false);", body)
        self.assertIn("if (mDevice == VK_NULL_HANDLE || mRetired.empty()) {", body)

    def test_nfr2_the_trim_thread_calls_it_while_the_renderer_runs(self):
        source = MEMORY.read_text(encoding="utf-8")
        run = source[source.index("void trim_run() {"):]
        run = run[:run.index("\n}\n")]
        self.assertIn("emu->freeFinishedRetired();", run)
        self.assertIn("constexpr std::chrono::seconds kTrimInterval{1};", source)
        install = source[source.index("void dsa_gfx_memory_install(void) {"):]
        self.assertIn("start_trim();", install[:install.index("\n}\n")])
        # It ends before the renderer is torn down.
        uninstall = source[source.index("void dsa_gfx_memory_uninstall(void) {"):]
        uninstall = uninstall[:uninstall.index("\n}\n")]
        self.assertLess(uninstall.index("stop_trim();"), uninstall.index("lock(s_mutex)"))

    def test_nfr2_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0025", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
