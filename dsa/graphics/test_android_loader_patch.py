"""Tests for the guest Vulkan driver's answers to Android's Vulkan loader. The loader is not
the Khronos one: it looks every command up once, right after vkCreateInstance, and calls some
of them on the app's behalf whatever Vulkan version the app asked for. Image test run
42539160677 showed what a driver that answered by the app's version does: com.android.hellovk
(Vulkan 1.0) died in vkGetPhysicalDeviceSurfaceFormatsKHR, which called address 0 for
vkGetPhysicalDeviceImageFormatProperties2."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH_DIR = HERE / "patches/guest/mesa"
LOOKUP_PATCH = next(PATCH_DIR.glob("0003-*.patch"))
EXTENSIONS_PATCH = next(PATCH_DIR.glob("0004-*.patch"))
DEVICE = "src/gfxstream/guest/vulkan/gfxstream_vk_device.cpp"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class AndroidLoaderPatchTests(unittest.TestCase):
    def test_pr3_a_vulkan_1_0_app_gets_the_core_commands_androids_loader_calls(self):
        added, removed = file_hunks(LOOKUP_PATCH, DEVICE)
        # The checked lookup stays first, so nothing it answers changes.
        self.assertIn("    return vk_instance_get_proc_addr(&instance->vk, &gfxstream_vk_instance_entrypoints, pName);",
                      removed)
        self.assertIn("        vk_instance_get_proc_addr(&instance->vk, &gfxstream_vk_instance_entrypoints, pName);",
                      added)
        # Only what it left unanswered, only on Android, and only with an instance.
        text = "\n".join(added)
        fallback = text.index("        func = vk_instance_get_proc_addr_unchecked(&instance->vk, pName);")
        self.assertLess(text.index("#if DETECT_OS_ANDROID"), fallback)
        self.assertLess(text.index("    if (!func && instance) {"), fallback)
        self.assertLess(fallback, text.index("#endif"))
        self.assertIn("    return func;", added)

    def test_pr3_the_driver_lists_only_the_device_extensions_vkcreatedevice_takes(self):
        # Run 42539160677: the Vulkan CTS enables every extension it is offered, and its
        # vkCreateDevice failed with VK_ERROR_EXTENSION_NOT_PRESENT before any case ran
        # ("Vulkan CTS subset: 0 of 97 cases passed"). Mesa's runtime, built with Android's
        # strict rules, refuses in vkCreateDevice what Android does not allow, and the driver
        # listed every extension the host has. The runtime's own listing applies that rule.
        added, removed = file_hunks(EXTENSIONS_PATCH, DEVICE)
        self.assertIn("        if (!pdevice->supported_extensions.extensions[i]) continue;", removed)
        self.assertIn("extern \"C\" VKAPI_ATTR VkResult VKAPI_CALL vk_common_EnumerateDeviceExtensionProperties(",
                      added)
        self.assertIn("    return vk_common_EnumerateDeviceExtensionProperties(physicalDevice, pLayerName,",
                      added)
        self.assertFalse([line for line in added if "vk_device_extensions[" in line])

    def test_pr3_the_build_keeps_androids_strict_extension_rules(self):
        # The listing and vkCreateDevice agree because both follow the same rule; turning the
        # rule off for one of them would make them disagree again.
        self.assertNotIn("android-strict", (HERE / "build.sh").read_text(encoding="utf-8"))

    def test_pr3_the_loader_patches_apply_after_the_patches_they_build_on(self):
        names = sorted(p.name for p in PATCH_DIR.glob("*.patch"))
        self.assertEqual([name[:5] for name in names[:4]], ["0001-", "0002-", "0003-", "0004-"])
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do',
                      (HERE / "fetch.sh").read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
