"""Tests for host patch 0042: a guest allocation that imports a ColorBuffer the renderer does not
know fails with a Vulkan error instead of ending the guest's stream.

The renderer asks its ColorBuffer table for the allocation info of the buffer an allocation
imports. After the window manager (and with it the renderer) restarted on the owner's PC, the
agent and SurfaceFlinger, which kept running, imported buffers the old renderer had made. The
lookup failed, vkAllocateMemory aborted ("Failed to get allocation info for ColorBuffer"), and
the abort ended that guest's whole stream. A bad request must take down no more than itself."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"


def read(path):
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


class UnknownColorBufferPatchTests(unittest.TestCase):
    def setUp(self):
        self.text = read(next(PATCHES.glob("0042-*.patch")))
        lines = self.text.splitlines()
        self.added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
        self.removed = [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")]

    def test_nfr5_a_missing_color_buffer_no_longer_aborts_the_stream(self):
        removed = "\n".join(self.removed)
        self.assertIn("GFXSTREAM_ABORT(FatalError(ABORT_REASON_OTHER))", removed)
        self.assertIn("Failed to get allocation info for ColorBuffer:", removed)
        self.assertNotIn("GFXSTREAM_ABORT", "\n".join(self.added))

    def test_nfr5_the_call_fails_with_a_vulkan_error_and_says_why(self):
        added = "\n".join(self.added)
        self.assertIn("return VK_ERROR_INVALID_EXTERNAL_HANDLE;", added)
        self.assertIn("ERR(", added)
        self.assertIn("ColorBuffer %u", added)

    def test_nfr5_a_snapshot_load_keeps_its_placeholder(self):
        # Only the abort inside the "not loading" branch is replaced; the placeholder code
        # after it stays as context.
        self.assertIn("// During snapshot load there could be invalidated references to", self.text)
        self.assertIn("if (mSnapshotState != SnapshotState::Loading) {", self.text)

    def test_nfr5_every_hunk_line_starts_with_a_diff_marker(self):
        # `git apply` on Windows rejects a context line that lost its leading space.
        in_hunk = False
        for line in self.text.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_nfr5_the_fetch_applies_0042_after_0041(self):
        self.assertIn(" 0041 0042 ", read(HOST / "fetch.sh"))
        self.assertIn('-Filter "0042-*.patch"', read(HOST / "fetch.ps1"))


if __name__ == "__main__":
    unittest.main()
