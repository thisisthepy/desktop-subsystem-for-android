"""Tests for the renderer's note on what an app's window surface costs in stencil.

HWUI asks for 8 stencil bits and no depth. The guest's ANGLE offers a stencil-only config only
when the GPU renders to VK_FORMAT_S8_UINT; otherwise each window surface gets D24S8 (four times
the bytes). No guest configuration chooses otherwise, so the renderer says once at its start
which the GPU allows, for the check on a real computer."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
INCLUDE = HERE / "include"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8")


class WindowStencilNoteTests(unittest.TestCase):
    def test_nfr1_the_renderer_says_which_stencil_buffer_a_window_surface_gets(self):
        memory = read(HOST / "dsa_gfx_memory.cpp")
        self.assertIn("say_window_stencil(ivk, emu->getPhysicalDevice());", memory)
        self.assertIn("renders(VK_FORMAT_S8_UINT)", memory)
        self.assertIn("VK_FORMAT_FEATURE_DEPTH_STENCIL_ATTACHMENT_BIT", memory)
        self.assertIn("dsa_gfx_log_line('N', note.c_str());", memory)

    def test_nfr1_the_note_names_the_format_and_its_bytes_a_pixel(self):
        header = read(INCLUDE / "dsa_stencil_format.h")
        self.assertIn("VK_FORMAT_S8_UINT, 1 byte", header)
        self.assertIn("VK_FORMAT_D24_UNORM_S8_UINT, 4 bytes", header)
        self.assertIn("stencil_format_test.cpp", read(HERE / "tests" / "test.sh"))


if __name__ == "__main__":
    unittest.main()
