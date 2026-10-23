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

    def test_pr3_process_pipe_uses_a_separate_hyper_v_socket(self):
        patch = PATCH.read_text()
        self.assertIn('#include "HvSocketStream.h"', patch)
        self.assertIn("sHvSocketStream", patch)
        self.assertIn("HOST_CONNECTION_HYPERV_SOCKET", patch)
        self.assertIn("kProcessPipePort", patch)


if __name__ == "__main__":
    unittest.main()
