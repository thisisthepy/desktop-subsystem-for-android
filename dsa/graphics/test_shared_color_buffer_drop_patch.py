"""Tests for host patch 0044: the renderer drops the shared ColorBuffers when no guest process
is left.

The guest numbers its gralloc buffers from 1 each time Android starts, and the renderer outlives
the guest. A buffer the last Android left behind was opened under a new buffer's number at the
old window's size, so SurfaceFlinger's image of the new size failed to import and the window
stayed black after a resize."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


class SharedColorBufferDropPatchTests(unittest.TestCase):
    def setUp(self):
        self.text = read(next(PATCHES.glob("0044-*.patch")))
        lines = self.text.splitlines()
        self.added = "\n".join(
            l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")
        )

    def test_fr4_a_shared_color_buffer_is_erased_whatever_its_reference_count(self):
        self.assertIn("int FrameBuffer::dropSharedColorBuffers()", self.added)
        self.assertIn("it->first & 0x80000000u", self.added)
        self.assertIn("m_colorbuffers.erase(it)", self.added)
        self.assertNotIn("refcount", self.added)

    def test_fr4_the_host_buffers_the_renderer_hands_out_stay(self):
        self.assertIn("} else {\n            ++it;", self.added)

    def test_fr4_the_backend_drops_them_when_the_last_process_pipe_ends(self):
        backend = read(HOST / "dsa_gfx_backend.cpp")
        self.assertIn("s_live_processes", backend)
        self.assertIn("dropSharedColorBuffers()", backend)

    def test_fr4_every_hunk_line_starts_with_a_diff_marker(self):
        in_hunk = False
        for line in self.text.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_fr4_the_fetch_applies_0044_after_0043(self):
        self.assertIn(" 0043 0044;", read(HOST / "fetch.sh"))
        self.assertIn('-Filter "0044-*.patch"', read(HOST / "fetch.ps1"))


if __name__ == "__main__":
    unittest.main()
