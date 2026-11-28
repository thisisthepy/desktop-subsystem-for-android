"""Tests for host patch 0049 and the trim after a guest's device goes away.

The renderer compacted its heaps only when no task window was left, so with any window open what
an app's device left behind stayed: about 18 MiB after one app opened and closed (the dev.37
report). Now a guest device destroyed through the decoder schedules one trim on the renderer's
trim thread, once no other device has gone for a moment: the renderer's command pool is trimmed
(patch 0049) and every heap compacted, and a note says how much private memory came back."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
INCLUDE = HERE / "include"
HOST = HERE / "host"
WINDOWS = HERE.parent / "host" / "platform" / "windows" / "src"


def read(path):
    return path.read_text(encoding="utf-8")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


class DeviceGoneTrimTests(unittest.TestCase):
    def setUp(self):
        self.patch = read(next(PATCHES.glob("0049-*.patch")))
        self.memory = read(HOST / "dsa_gfx_memory.cpp")

    def test_nfr1_the_command_pool_is_trimmed_under_the_emulations_lock(self):
        body = added(self.patch)
        self.assertIn("void trimCommandPool();", body)
        self.assertIn("std::lock_guard<std::mutex> lock(mMutex);", body)
        self.assertIn("mDvk->vkTrimCommandPool(mDevice, mCommandPool, 0);", body)
        self.assertIn("!mDvk->vkTrimCommandPool", body)

    def test_nfr1_a_guest_device_destroyed_schedules_the_trim(self):
        destroy = self.memory[self.memory.index("counted_destroy_through_decoder(VkDevice device"):]
        destroy = destroy[:destroy.index("\n}\n")]
        self.assertIn("s_gone.deviceGone(", destroy)
        # The renderer's own device, made again after a GPU reset, is not a guest's.
        own = self.memory[self.memory.index("counted_destroy_through_instance(VkDevice device"):]
        own = own[:own.index("\n}\n")]
        self.assertNotIn("s_gone", own)

    def test_nfr1_the_trim_runs_on_the_trim_thread_and_says_what_it_gave_back(self):
        run = self.memory[self.memory.index("void trim_run()"):]
        run = run[:run.index("\nvoid start_trim()")]
        self.assertIn("trim_after_devices_gone();", run)
        trim = self.memory[self.memory.index("void trim_after_devices_gone()"):]
        trim = trim[:trim.index("\nvoid trim_run()")]
        self.assertIn("emu->trimCommandPool();", trim)
        self.assertIn("dsa_gfx_compact_heaps();", trim)
        self.assertIn("dsa_gfx_log_line('N', line);", trim)
        self.assertIn("given back", trim)
        # Windows are open: the working set is not emptied.
        self.assertNotIn("EmptyWorkingSet", trim)

    def test_nfr1_every_heap_is_compacted_on_each_host(self):
        heaps = read(HOST / "dsa_gfx_heap_trim.cpp")
        self.assertIn("HeapCompact(heaps[i], 0);", heaps)
        self.assertIn("malloc_trim(0);", heaps)
        self.assertIn("malloc_zone_pressure_relief(nullptr, 0);", heaps)
        self.assertIn("K32GetProcessMemoryInfo", heaps)
        self.assertIn("dsa_gfx_heap_trim.cpp", read(HOST / "CMakeLists.txt"))

    def test_nfr1_the_policy_has_its_test(self):
        self.assertIn("kSettle{2000}", read(INCLUDE / "dsa_device_gone_trim.h"))
        self.assertIn("device_gone_trim_test.cpp", read(HERE / "tests" / "test.sh"))

    def test_nfr1_the_note_reaches_the_window_managers_log(self):
        log = read(WINDOWS / "compositor" / "log.rs")
        self.assertIn("b'N' => Some(format!(\"graphics renderer: {message}\")),", log)
        self.assertIn("b'W' | b'E' | b'F' | b'N'", log)

    def test_pr3_every_context_line_starts_with_a_space(self):
        in_hunk = False
        for line in self.patch.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_pr3_the_fetch_applies_0049_after_0048(self):
        self.assertIn(" 0048 0049", read(HOST / "fetch.sh"))
        fetch = read(HOST / "fetch.ps1")
        self.assertLess(fetch.index('"0048-*.patch"'), fetch.index('"0049-*.patch"'))


if __name__ == "__main__":
    unittest.main()
