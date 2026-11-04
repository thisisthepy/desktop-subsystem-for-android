"""Tests for the standalone guest graphics transport build."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
BUILD_SCRIPT = HERE / "build.sh"
CMAKE_FILE = HERE / "cmake" / "CMakeLists.txt"
VULKAN_FILE = HERE / "cmake" / "vulkan.cmake"


class GraphicsBuildTests(unittest.TestCase):
    def test_pr3_build_script_invokes_cmake_for_guest_transport(self):
        script = BUILD_SCRIPT.read_text()
        self.assertIn("android.toolchain.cmake", script)
        self.assertIn('-DANDROID_ABI="$android_abi"', script)
        self.assertIn("OpenglSystemCommon", script)
        self.assertIn("vulkan.ranchu", script)

    def test_pr3_cmake_defines_guest_library_and_stub_linkage(self):
        cmake = CMAKE_FILE.read_text()
        self.assertIn("add_library(OpenglSystemCommon SHARED", cmake)
        self.assertIn("HvSocketStream.cpp", cmake)
        self.assertIn("-Wl,-z,undefs", cmake)
        self.assertIn("add_stub_library", cmake)

    def test_pr3_vulkan_ranchu_is_built_from_the_android_sources(self):
        vulkan = VULKAN_FILE.read_text()
        self.assertIn("include(${CMAKE_CURRENT_SOURCE_DIR}/vulkan.cmake)", CMAKE_FILE.read_text())
        self.assertIn("add_library(vulkan.ranchu SHARED", vulkan)
        self.assertIn('OUTPUT_NAME "vulkan.ranchu"', vulkan)
        for source in [
            "gfxstream_vk_android.cpp",
            "GrallocMinigbm.cpp",
            "renderControl_enc/GfxStreamRenderControl.cpp",
            "OpenglSystemCommon/HvSocketStream.cpp",
            "gfxstream_vk_entrypoints.c",
            "libvulkan_lite_runtime.a",
        ]:
            self.assertIn(source, vulkan)

    def test_pr3_vulkan_ranchu_links_with_every_symbol_resolved(self):
        # The driver once linked with undefined symbols allowed and could not load on
        # the device at all.
        vulkan = VULKAN_FILE.read_text()
        self.assertIn("-Wl,--no-undefined", vulkan)
        self.assertNotIn("-z,undefs", vulkan)

    def test_pr3_vulkan_ranchu_exports_only_the_hal_module(self):
        vulkan = VULKAN_FILE.read_text()
        self.assertIn("vulkan-android.sym", vulkan)

    def test_pr3_build_script_builds_mesa_with_meson_for_android(self):
        script = BUILD_SCRIPT.read_text()
        self.assertIn("meson setup", script)
        self.assertIn("-Dplatforms=android", script)
        self.assertIn("-Dvulkan-drivers=gfxstream", script)
        self.assertIn('-DMESA_BUILD_DIR="$mesa_build/out"', script)

    def test_fr27_the_guest_transport_builds_for_arm64_too(self):
        script = BUILD_SCRIPT.read_text()
        self.assertIn('guest_arch="${DSA_GUEST_ARCH:-x86_64}"', script)
        self.assertIn("arm64) android_abi=arm64-v8a; android_triple=aarch64-linux-android", script)
        self.assertIn("--target=${android_triple}34", script)
        self.assertIn("arm64) mesa_cpu=aarch64", script)
        # Each architecture has its own Mesa build, so one never links the other's objects.
        self.assertIn('mesa_build="$scratch/build-mesa-$guest_arch"', script)

    def test_fr27_sse41_helpers_are_built_and_linked_only_for_x86_64(self):
        script = BUILD_SCRIPT.read_text()
        self.assertIn('[[ "$guest_arch" == x86_64 ]] && mesa_sse41=src/util/libmesa_util_sse41.a', script)
        self.assertNotIn(" src/util/libmesa_util_sse41.a \\", script)
        vulkan = VULKAN_FILE.read_text()
        self.assertIn('if(ANDROID_ABI STREQUAL "x86_64")', vulkan)


if __name__ == "__main__":
    unittest.main()
