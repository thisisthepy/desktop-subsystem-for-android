"""Regression tests for the gfxstream Hyper-V socket transport patch."""

import unittest
from pathlib import Path


PATCH = Path(__file__).resolve().parent / "patches" / "0001-add-hyper-v-socket-transport.patch"


class HvSocketPatchTests(unittest.TestCase):
    def test_pr3_guest_gfxstream_uses_af_vsock_when_selected(self):
        patch = PATCH.read_text()
        self.assertIn("class HvSocketStream", patch)
        self.assertIn("AF_VSOCK", patch)
        self.assertIn("VMADDR_CID_HOST", patch)
        self.assertIn('transport == "hvsocket"', patch)
        self.assertIn("HvSocketStream.cpp", patch)


if __name__ == "__main__":
    unittest.main()
