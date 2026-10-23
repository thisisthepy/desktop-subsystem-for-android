"""Regression tests for the gfxstream Hyper-V socket transport patch."""

import unittest
from pathlib import Path


PATCH_0001 = Path(__file__).resolve().parent / "patches" / "0001-add-hyper-v-socket-transport.patch"
PATCH_0002 = Path(__file__).resolve().parent / "patches" / "0002-add-hyper-v-vulkan-and-gralloc.patch"


class HvSocketPatchTests(unittest.TestCase):
    def test_pr3_guest_gfxstream_uses_af_vsock_when_selected(self):
        patch = PATCH_0001.read_text()
        self.assertIn("class HvSocketStream", patch)
        self.assertIn("AF_VSOCK", patch)
        self.assertIn("VMADDR_CID_HOST", patch)
        self.assertIn('transport == "hvsocket"', patch)
        self.assertIn("HvSocketStream.cpp", patch)

    def test_pr3_process_pipe_uses_a_separate_hyper_v_socket(self):
        patch = PATCH_0001.read_text()
        self.assertIn('#include "HvSocketStream.h"', patch)
        self.assertIn("sHvSocketStream", patch)
        self.assertIn("HOST_CONNECTION_HYPERV_SOCKET", patch)
        self.assertIn("kProcessPipePort", patch)

    def test_pr3_connection_manager_supports_hyper_v_socket(self):
        patch = PATCH_0002.read_text()
        self.assertIn("GFXSTREAM_TRANSPORT_HYPERV_SOCKET", patch)
        self.assertIn('#include "HvSocketStream.h"', patch)
        self.assertIn("HvSocketStream(STREAM_BUFFER_SIZE, 5566)", patch)
        self.assertIn("HvSocketStream(4096, 5567)", patch)

    def test_pr3_render_control_selects_hvsocket_from_property(self):
        patch = PATCH_0002.read_text()
        self.assertIn('transport == "hvsocket"', patch)
        self.assertIn("GFXSTREAM_TRANSPORT_HYPERV_SOCKET", patch)

    def test_pr3_gralloc_creates_host_color_buffer_without_virtio_gpu(self):
        patch = PATCH_0002.read_text()
        self.assertIn("rcCreateColorBuffer", patch)
        self.assertIn("rcCloseColorBuffer", patch)
        self.assertIn("mBufferIdToColorBuffer", patch)
        self.assertIn("cb->hostHandle", patch)
        self.assertIn("Hyper-V", patch)


if __name__ == "__main__":
    unittest.main()
