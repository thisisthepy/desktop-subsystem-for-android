"""Tests for host patch 0046 and the presentation's side of it: a composition never writes a
ColorBuffer the presentation is still copying into a task window.

The hwcomposer kept two ColorBuffers per task, each as large as the task can be (24 MiB for a
task of 3072x2048), only so that it never composed into the one being copied. Now the
presentation counts each ColorBuffer's frames that wait for or are in their copy, and
rcComposeWithoutPost waits a few milliseconds for that count to reach zero, or answers that the
target is busy. A hwcomposer that sees the host's extension composes every frame into one
ColorBuffer while its window keeps up (dsa_compose_guard.h)."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
HOST = HERE / "host"
INCLUDE = HERE / "include"


def read(path):
    return path.read_text(encoding="utf-8").replace("\r\n", "\n")


class ComposeGuardPatchTests(unittest.TestCase):
    def setUp(self):
        self.text = read(next(PATCHES.glob("0046-*.patch")))
        lines = self.text.splitlines()
        self.added = "\n".join(
            l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")
        )
        self.display = read(HOST / "dsa_gfx_display.cpp")
        self.header = read(INCLUDE / "dsa_compose_guard.h")

    def test_nfr1_only_the_render_control_changes(self):
        files = re.findall(r"^diff --git a/(\S+) ", self.text, flags=re.M)
        self.assertEqual(files, ["host/RenderControl.cpp"])

    def test_nfr1_a_busy_target_is_not_composed_into(self):
        self.assertIn("dsa_compose_guard_fn dsa_compose_target_guard = nullptr;", self.added)
        self.assertIn(
            "if (version == 2 && target != 0 && !guard(target, DSA_COMPOSE_GUARD_WAIT_MS)) {",
            self.added)
        self.assertIn("return DSA_COMPOSE_TARGET_BUSY;", self.added)
        # Defined outside gfxstream's namespace, so it is the one the header declares.
        before_namespace = self.added.split("namespace gfxstream {")[0]
        self.assertIn("dsa_compose_target_guard = nullptr;", before_namespace)

    def test_nfr1_the_host_lists_the_extension_only_while_something_tracks_its_copies(self):
        self.assertIn("if (name == GL_EXTENSIONS && dsa_compose_target_guard) {", self.added)
        self.assertIn("glStr += DSA_COMPOSE_GUARD_EXTENSION;", self.added)

    def test_nfr1_an_older_hwcomposer_takes_a_busy_answer_as_a_failed_composition(self):
        # It treats any answer above 0 as composed.
        self.assertIn("#define DSA_COMPOSE_TARGET_BUSY (-2)", self.header)

    def test_nfr1_every_frame_queued_for_a_copy_is_counted_once_and_let_go_once(self):
        self.assertIn("dsa_compose_target_guard = compose_guard;", self.display)
        # Queued when the guest posts it; a newer frame of the same display replaces it.
        self.assertIn("copy_queued(color_buffer);", self.display)
        self.assertIn("copy_finished(dropped);", self.display)
        # Let go once the copy has read it, before the window manager shows the frame.
        self.assertIn("hold.release();\n    report_buffers(", self.display)
        self.assertIn("CopyHold hold(frame.color_buffer);", self.display)
        # Frames dropped with their display or with the presentation.
        self.assertIn("copy_finished(pending->second.color_buffer);", self.display)
        self.assertIn("copy_finished(entry.second.color_buffer);", self.display)

    def test_nfr1_the_renderer_logs_the_buffers_each_display_holds(self):
        self.assertIn("Display %u's frames came in %zu ColorBuffer(s)", self.display)

    def test_nfr1_the_fetch_applies_0046_after_0045(self):
        self.assertIn(" 0045 0046;", read(HOST / "fetch.sh"))
        self.assertIn('-Filter "0046-*.patch"', read(HOST / "fetch.ps1"))


if __name__ == "__main__":
    unittest.main()
