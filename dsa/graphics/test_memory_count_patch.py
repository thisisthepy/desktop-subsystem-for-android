"""Tests for the renderer's memory count following every Vulkan device.

On the owner's PC (Intel Arc, Windows 11, v0.2.3, 2026-11-12) the renderer counted
"GPU-only 94 MiB in 21" before a GPU reset and "GPU-only 0 MiB in 0" ever after, while its
private bytes grew from 399 to 983 MiB. The count wrapped the allocate and free entries of
the renderer's device dispatch table once, at start; the device made again after the reset
fills that table anew from the driver, so nothing was counted from then on. A guest
process's device has a table of its own, which was never wrapped, so what the guests
allocate (CPU-mapped memory read 0 in every report) was never counted either.

Patch 0023 hands every device dispatch table gfxstream fills to a hook the embedder sets, and
dsa_gfx_memory.cpp wraps each one. The run on a GPU (software Vulkan in CI) is
nfr1_the_renderer_counts_every_device_after_a_lost_device in
dsa/host/platform/windows/tests/window_manager.rs."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0023-*.patch"))
MEMORY = HERE / "host" / "dsa_gfx_memory.cpp"


def file_hunks(patch, name):
    """The added lines and the context lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
            after = [l[1:] for l in lines
                     if (l.startswith("+") and not l.startswith("+++")) or l.startswith(" ")]
            return added, after
    raise AssertionError(f"{patch.name} does not change {name}")


class DeviceDispatchHookPatchTests(unittest.TestCase):
    def test_nfr1_every_filled_device_table_goes_to_the_hook(self):
        for name, count in (("host/vulkan/VkCommonOperations.cpp", 2),
                            ("host/vulkan/VkDecoderGlobalState.cpp", 1)):
            _, after = file_hunks(PATCH, name)
            fills = [i for i, l in enumerate(after) if "init_vulkan_dispatch_from_device(" in l]
            self.assertEqual(len(fills), count, name)
            for at in fills:
                following = "\n".join(after[at + 1:at + 3])
                self.assertIn("runDeviceDispatchHook(", following, after[at])

    def test_nfr1_the_device_made_again_after_a_loss_is_wrapped_again(self):
        _, after = file_hunks(PATCH, "host/vulkan/VkCommonOperations.cpp")
        text = "\n".join(after)
        self.assertIn("init_vulkan_dispatch_from_device(mIvk, mDevice, mDvk);", text)
        at = text.index("init_vulkan_dispatch_from_device(mIvk, mDevice, mDvk);")
        self.assertIn("runDeviceDispatchHook(mDevice, mDvk);", text[at:at + 300])

    def test_nfr1_the_hook_is_declared_for_the_embedder(self):
        added, _ = file_hunks(PATCH, "host/vulkan/VulkanDispatch.h")
        text = "\n".join(added)
        self.assertIn("using DeviceDispatchHook = void (*)(VkDevice device, VulkanDispatch* "
                      "dispatch);", text)
        self.assertIn("void setDeviceDispatchHook(DeviceDispatchHook hook);", text)
        self.assertIn("void runDeviceDispatchHook(VkDevice device, VulkanDispatch* dispatch);",
                      text)


class MemoryCountTests(unittest.TestCase):
    def setUp(self):
        self.source = MEMORY.read_text(encoding="utf-8")

    def test_nfr1_the_count_wraps_every_table_the_hook_is_given(self):
        self.assertIn("gfxstream::vk::setDeviceDispatchHook(wrap_device_table);", self.source)
        # The renderer's own device was made before the hook was set.
        self.assertIn("wrap_device_table(emu->getDevice(), emu->getDeviceDispatch());",
                      self.source)
        # Each device calls its own driver entries.
        self.assertIn("std::map<VkDevice, DeviceEntries> s_devices;", self.source)

    def test_nfr1_a_destroyed_device_takes_its_allocations_with_it(self):
        for table, wrapper in (("gfxstream::vk::vkDispatch()", "counted_destroy_through_decoder"),
                               ("ivk", "counted_destroy_through_instance")):
            self.assertIn(f"wrap_destroy({table}, {wrapper},", self.source)
        forget = self.source[self.source.index("void forget_device(VkDevice device) {"):]
        forget = forget[:forget.index("\n}\n")]
        self.assertIn("s_live.erase(it)", forget)
        self.assertIn("s_devices.erase(device);", forget)

    def test_nfr1_the_wrapped_entries_keep_forwarding_after_the_count_stops(self):
        uninstall = self.source[self.source.index("void dsa_gfx_memory_uninstall(void) {"):]
        uninstall = uninstall[:uninstall.index("\n}\n")]
        self.assertNotIn("s_devices.clear()", uninstall)
        self.assertIn("s_counting = false;", uninstall)

    def test_nfr1_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0023", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
