"""Regression tests for the gfxstream Hyper-V socket transport patch."""

import unittest
from pathlib import Path


PATCH_0001 = Path(__file__).resolve().parent / "patches" / "0001-add-hyper-v-socket-transport.patch"
PATCH_0002 = Path(__file__).resolve().parent / "patches" / "0002-add-hyper-v-vulkan-and-gralloc.patch"
PATCH_0003 = Path(__file__).resolve().parent / "patches" / "0003-copy-host-visible-memory-without-shared-memory.patch"
PATCH_0005 = Path(__file__).resolve().parent / "patches" / "0005-give-each-task-window-a-display-and-share-color-buffers.patch"
HOST_FETCH = Path(__file__).resolve().parent / "host" / "fetch.ps1"
SHARED_HANDLE = Path(__file__).resolve().parent / "include" / "dsa_shared_color_buffer.h"
VULKAN_CMAKE = Path(__file__).resolve().parent / "cmake" / "vulkan.cmake"
HOST_BACKEND = Path(__file__).resolve().parent / "host" / "dsa_gfx_backend.cpp"
FETCH = Path(__file__).resolve().parent / "fetch.sh"


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

    def test_pr3_android_driver_selects_hvsocket_from_property(self):
        # On Android the Vulkan driver takes its transport from gfxstream's render
        # control, not from Mesa's Linux stub.
        patch = PATCH_0001.read_text()
        self.assertIn("guest/renderControl_enc/GfxStreamRenderControl.cpp", patch)
        self.assertIn("return GFXSTREAM_TRANSPORT_HYPERV_SOCKET;", patch)

    def test_pr3_gralloc_creates_host_color_buffer_without_virtio_gpu(self):
        patch = PATCH_0002.read_text()
        self.assertIn("rcCreateColorBuffer", patch)
        self.assertIn("rcCloseColorBuffer", patch)
        self.assertIn("mBufferIdToColorBuffer", patch)
        self.assertIn("cb->hostHandle", patch)
        self.assertIn("Hyper-V", patch)


    def test_fr3_every_process_reaches_the_same_host_color_buffer(self):
        # The app draws a buffer, SurfaceFlinger and the hwcomposer show it: all three must
        # reach one host ColorBuffer, so its handle comes from the buffer's ID, opened if
        # another process made it and created only if none has.
        patch = PATCH_0002.read_text()
        self.assertIn('#include "dsa_shared_color_buffer.h"', patch)
        self.assertIn("dsa_shared_color_buffer_handle(bufferId)", patch)
        self.assertIn("rcOpenColorBuffer2(rcEnc, hostHandle) != 0", patch)
        self.assertIn("rcCreateColorBufferWithHandle(rcEnc, width, height, kGlFormatRGBA, hostHandle)", patch)
        header = SHARED_HANDLE.read_text()
        self.assertIn("DSA_SHARED_COLOR_BUFFER_BIT 0x80000000u", header)
        self.assertIn("${CMAKE_CURRENT_SOURCE_DIR}/../include", VULKAN_CMAKE.read_text())

    def test_fr3_the_host_accepts_a_color_buffer_two_processes_create(self):
        # Two processes can race to create the shared ColorBuffer; the host must not abort.
        patch = PATCH_0005.read_text()
        self.assertIn("-            GFXSTREAM_ABORT(FatalError(ABORT_REASON_OTHER));", patch)
        self.assertIn("+            return;", patch)

    def test_fr3_the_host_gives_every_task_window_a_display(self):
        patch = PATCH_0005.read_text()
        self.assertIn("kDsaMaxDisplays = 64", patch)
        self.assertIn('-Filter "0005-*.patch"', HOST_FETCH.read_text())

    def test_pr3_host_visible_memory_works_without_shared_memory(self):
        # A Hyper-V VM has no goldfish address space and no virtio-gpu blobs, so the guest
        # keeps a copy of host-visible memory and the stream carries it: flushed before a
        # submit, read back after a wait.
        patch = PATCH_0003.read_text()
        self.assertIn("return mFeatureInfo.hasDirectMem || mFeatureInfo.hasVirtioGpuNext", patch)
        self.assertIn("aligned_alloc(kLargestPageSize", patch)
        self.assertIn("flushShadowedMemory(static_cast<VkEncoder*>(context), device);", patch)
        self.assertIn("invalidateShadowedMemory(vkEncoder, device);", patch)
        self.assertIn("VK_MEMORY_PROPERTY_HOST_CACHED_BIT", patch)
        self.assertIn('"$patch_dir"/0003-*.patch', FETCH.read_text())

    def test_pr3_host_renderer_does_not_expect_shared_memory(self):
        # The host decoder copies memory from the stream only when neither GlDirectMem nor
        # VirtioGpuNext is on, and the guest decides from the same features.
        backend = HOST_BACKEND.read_text()
        self.assertIn("STREAM_RENDERER_PARAM_RENDERER_FEATURES", backend)
        self.assertIn("VirtioGpuNext:disabled", backend)

if __name__ == "__main__":
    unittest.main()
