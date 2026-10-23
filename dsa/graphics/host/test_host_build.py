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
        self.assertIn("add_custom_target(dist_host_gfxstream", cmake)

    def test_pr3_host_fetch_script_pins_upstream_and_dependencies(self):
        script = FETCH_SCRIPT.read_text(encoding="utf-8")
        self.assertIn("d047a57228332d995d36600792fa9ccc26cf8ae6", script)
        self.assertIn("3ff20aed307f55bc4ae865373d07bf76c9efa43c", script)
        self.assertIn("https://android.googlesource.com/platform/external/angle", script)


if __name__ == "__main__":
    unittest.main()
