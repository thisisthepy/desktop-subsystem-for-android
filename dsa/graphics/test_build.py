"""Tests for the standalone guest graphics transport build."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
BUILD_SCRIPT = HERE / "build.sh"
CMAKE_FILE = HERE / "cmake" / "CMakeLists.txt"


class GraphicsBuildTests(unittest.TestCase):
    def test_pr3_build_script_invokes_cmake_for_guest_transport(self):
        script = BUILD_SCRIPT.read_text()
        self.assertIn("android.toolchain.cmake", script)
        self.assertIn("ANDROID_ABI=x86_64", script)
        self.assertIn("OpenglSystemCommon", script)

    def test_pr3_cmake_defines_guest_library_and_stub_linkage(self):
        cmake = CMAKE_FILE.read_text()
        self.assertIn("add_library(OpenglSystemCommon SHARED", cmake)
        self.assertIn("HvSocketStream.cpp", cmake)
        self.assertIn("-Wl,-z,undefs", cmake)
        self.assertIn("add_stub_library", cmake)


if __name__ == "__main__":
    unittest.main()
