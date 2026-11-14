"""Tests for the copy of what a CPU wrote into a gralloc buffer up to its host ColorBuffer. A
Hyper-V VM has no virtio-gpu device, so a video decoder's pictures, a camera's frames and a
layer drawn in software were only in the guest's memory: a video played its sound over a
green picture (media boot check, Image test run 42724126311). The host makes YUV ColorBuffers
(patches/0019), the guest Vulkan driver copies a buffer up before a submit
(patches/guest/mesa/0008), and the hwcomposer copies every layer it is handed. The rules are
in include/dsa_cpu_upload.h, tested by tests/cpu_upload_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
HOST_PATCH = next((HERE / "patches").glob("0019-*.patch"))
GUEST_PATCH = next((HERE / "patches/guest/mesa").glob("0008-*.patch"))
SHARED = HERE / "include/dsa_shared_color_buffer.h"
TESTS = HERE / "tests/test.sh"


def file_hunks(patch, name):
    """The added lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return [l[1:] for l in part.splitlines() if l.startswith("+") and not l.startswith("+++")]
    raise AssertionError(f"{patch.name} does not change {name}")


class CpuUploadPatchTests(unittest.TestCase):
    def test_pr4_the_host_and_guest_agree_on_the_yuv_formats_and_extension(self):
        shared = SHARED.read_text(encoding="utf-8")
        host = "\n".join(file_hunks(HOST_PATCH, "host/RenderControl.cpp"))
        name = re.search(r'#define DSA_YUV_COLOR_BUFFERS_EXTENSION "([^"]+)"', shared).group(1)
        self.assertIn(f'kDsaYuvColorBuffers = "{name}"', host)
        three = re.search(r"#define DSA_COLOR_BUFFER_YUV420_THREE_PLANE (0x[0-9A-Fa-f]+)u", shared).group(1)
        two = re.search(r"#define DSA_COLOR_BUFFER_YUV420_TWO_PLANE (0x[0-9A-Fa-f]+)u", shared).group(1)
        self.assertIn(f"kDsaYuv420ThreePlane = {three};", host)
        self.assertIn(f"kDsaYuv420TwoPlane = {two};", host)

    def test_nfr5_the_host_lists_and_makes_yuv_colorbuffers_only_when_its_gpu_can(self):
        # A ColorBuffer the host cannot make ends the renderer, so the extension and the YUV
        # format follow the GPU's support, and a host without it gives RGBA as before.
        host = "\n".join(file_hunks(HOST_PATCH, "host/RenderControl.cpp"))
        self.assertIn("fb->supportsDsaYuvColorBuffers()", host)
        self.assertIn("        if (fb->supportsDsaYuvColorBuffers()) {\n            frameworkFormat = yuv;", host)
        vk = "\n".join(file_hunks(HOST_PATCH, "host/vulkan/VkCommonOperations.cpp"))
        self.assertIn("VK_FORMAT_FEATURE_SAMPLED_IMAGE_BIT | VK_FORMAT_FEATURE_TRANSFER_DST_BIT", vk)

    def test_nfr5_the_host_checks_the_packed_planes_against_its_image(self):
        fb = "\n".join(file_hunks(HOST_PATCH, "host/FrameBuffer.cpp"))
        self.assertIn("vk::getFormatTransferInfo(vkFormat, width, height, &expected, nullptr)", fb)
        self.assertIn("expected != size", fb)

    def test_pr4_the_guest_driver_copies_up_before_each_submit_outside_surfaceflinger(self):
        tracker = "\n".join(file_hunks(GUEST_PATCH, "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"))
        # Before the submit reaches the host.
        self.assertIn("+    uploadForGpu();\n     flushStagingStreams(context, queue, submitCount, pSubmits);",
                      GUEST_PATCH.read_text(encoding="utf-8"))
        self.assertIn('strcmp(getprogname(), "surfaceflinger") == 0', tracker)
        self.assertIn("mGralloc->wantsCpuUpload(mGralloc->getNativeHandle(info.ahw))", tracker)
        self.assertIn("    mCpuUploads.erase((uint64_t)mem);", tracker)

    def test_pr4_the_guest_reads_the_planes_through_a_gralloc_lock(self):
        gralloc = "\n".join(file_hunks(GUEST_PATCH, "src/gfxstream/guest/android/GrallocMinigbm.cpp"))
        self.assertIn("AHardwareBuffer_lock(ahb, AHARDWAREBUFFER_USAGE_CPU_READ_OFTEN", gralloc)
        self.assertIn("dsa_shared_color_buffer_create_format(", gralloc)
        self.assertIn("mesa_logd(", gralloc)

    def test_pr4_the_header_test_runs_with_the_graphics_tests(self):
        self.assertIn('"$here/cpu_upload_test.cpp"', TESTS.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
