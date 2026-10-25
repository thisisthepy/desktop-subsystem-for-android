"""Tests for the standalone host gfxstream renderer build scripts and CMake glue."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
BUILD_SCRIPT = HERE / "build.ps1"
FETCH_SCRIPT = HERE / "fetch.ps1"
CMAKE_FILE = HERE / "CMakeLists.txt"


class HostGraphicsBuildTests(unittest.TestCase):
    def test_pr3_host_build_script_invokes_cmake_for_gfxstream_backend(self):
        script = BUILD_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("Ninja", script)
        self.assertIn("clang-cl", script)
        self.assertIn("gfxstream_backend", script)
        self.assertIn("dist_host_gfxstream", script)

    def test_pr3_host_cmake_glue_enforces_vulkan_and_standalone_aemu(self):
        cmake = CMAKE_FILE.read_text(encoding="utf-8")
        self.assertIn("VK_USE_PLATFORM_WIN32_KHR", cmake)
        self.assertIn("AEMU_COMMON_BUILD_CONFIG", cmake)
        self.assertIn("add_library(flatbuffers INTERFACE)", cmake)
        self.assertIn("add_library(libdrm_headers INTERFACE)", cmake)
        self.assertIn("add_library(gfxstream_egl_headers INTERFACE)", cmake)
        self.assertIn("emugl_config_stub.cpp", cmake)
        self.assertIn("add_custom_target(dist_host_gfxstream", cmake)

    def test_pr3_host_fetch_script_pins_upstream_and_dependencies(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("d047a57228332d995d36600792fa9ccc26cf8ae6", script)
        self.assertIn("3ff20aed307f55bc4ae865373d07bf76c9efa43c", script)
        self.assertIn("https://android.googlesource.com/platform/external/angle", script)

    def test_fr3_host_fetch_applies_the_patch_that_lends_out_the_vulkan_queue(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn('"0004-*.patch"', script)
        # The script is in dsa/graphics/host, three levels below the repository root.
        self.assertIn('"$PSScriptRoot/../../.."', script)
        self.assertIn("throw", script)
        patch = (HERE.parent / "patches" / "0004-expose-the-vulkan-queue-to-the-host-backend.patch")
        text = patch.read_text(encoding="utf-8")
        self.assertIn("host/vulkan/VkCommonOperations.h", text)
        self.assertIn("getQueueLock", text)

    def test_fr3_each_display_is_copied_into_its_window_on_the_gpu(self):
        header = (HERE / "dsa_gfx_backend.h").read_text(encoding="utf-8")
        self.assertIn("dsa_gfx_set_display_callbacks", header)
        self.assertIn("dsa_gfx_get_adapter_luid", header)
        source = (HERE / "dsa_gfx_display.cpp").read_text(encoding="utf-8")
        # The frame goes into the window's Direct3D texture without a trip through the CPU.
        self.assertIn("VK_EXTERNAL_MEMORY_HANDLE_TYPE_D3D11_TEXTURE_BIT", source)
        self.assertIn("vkCmdBlitImage", source)
        self.assertNotIn("readColorBuffer", source)
        self.assertIn("setDisplayColorBuffer", source)
        self.assertIn("dsa_gfx_display.cpp", CMAKE_FILE.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
