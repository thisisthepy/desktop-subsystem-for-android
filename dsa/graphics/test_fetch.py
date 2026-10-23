"""Tests for the reproducible gfxstream source checkout."""

import unittest
from pathlib import Path


HERE = Path(__file__).resolve().parent
FETCH = HERE / "fetch.sh"


class GfxstreamFetchTests(unittest.TestCase):
    def test_pr3_fetches_the_pinned_upstream_revision_before_applying_dsa_patches(self):
        script = FETCH.read_text()
        self.assertIn("d047a57228332d995d36600792fa9ccc26cf8ae6", script)
        self.assertIn(
            "https://android.googlesource.com/platform/hardware/google/gfxstream",
            script,
        )
        self.assertIn("git -C \"$source\" checkout --detach \"$revision\"", script)
        self.assertIn("git -C \"$source\" apply --recount --check", script)
        self.assertIn("git -C \"$source\" apply --recount", script)


if __name__ == "__main__":
    unittest.main()
