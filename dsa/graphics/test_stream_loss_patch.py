"""Tests for the guest Vulkan driver's handling of a stream to the host renderer that ended:
the renderer restarted in its own process, or ended this guest's stream. The driver reports a
lost device instead of ending the process, and a new instance connects again."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
MESA_PATCH = next((HERE / "patches/guest/mesa").glob("*.patch"))
GFXSTREAM_PATCH = next((HERE / "patches/guest/gfxstream").glob("*.patch"))
FETCH = HERE / "fetch.sh"
HEADER = HERE / "include/dsa_lost_stream.h"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    parts = re.split(r"^diff --git ", text, flags=re.M)
    for part in parts:
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class StreamLossPatchTests(unittest.TestCase):
    def test_nfr5_a_failed_read_no_longer_ends_the_process(self):
        added, removed = file_hunks(MESA_PATCH, "src/gfxstream/guest/vulkan_enc/VulkanStreamGuest.cpp")
        self.assertIn("        abort();", removed)
        self.assertNotIn("abort();", "\n".join(added))
        self.assertIn('    lose("read");', added)
        self.assertIn("    dsa::LostStream::zero(buffer, size);", added)
        # Writes on a dead stream go nowhere instead of through a null buffer.
        self.assertIn("    return mLoss.scratch(size);", added)

    def test_nfr5_every_call_that_returns_a_result_reports_the_lost_device(self):
        added, _ = file_hunks(MESA_PATCH, "src/gfxstream/guest/vulkan_enc/VkEncoder.cpp")
        overrides = [l for l in added if re.fullmatch(
            r"\s*if \(stream->isLost\(\)\) \w+_VkResult_return = VK_ERROR_DEVICE_LOST;", l)]
        self.assertEqual(len(overrides), 106)

    def test_nfr5_a_lost_stream_takes_no_sequence_number_the_renderer_would_wait_for(self):
        added, _ = file_hunks(MESA_PATCH, "src/gfxstream/guest/vulkan_enc/VkEncoder.cpp")
        guarded = [l for l in added if "seqno = stream->isLost() ? 0 : ResourceTracker::nextSeqno();" in l]
        self.assertEqual(len(guarded), 215)
        added, _ = file_hunks(MESA_PATCH,
                              "src/gfxstream/guest/vulkan_enc/vkQueueFlushCommandsGOOGLE_encode_impl.cpp.inl")
        self.assertIn("uint32_t seqno = stream->isLost() ? 0 : ResourceTracker::nextSeqno();", added)

    def test_nfr5_a_new_instance_starts_over_with_a_new_connection(self):
        added, _ = file_hunks(MESA_PATCH, "src/gfxstream/guest/vulkan/gfxstream_vk_device.cpp")
        text = "\n".join(added)
        # The instance-level calls (vkCreateInstance, the enumerations) all set up the process.
        self.assertIn("    startOverAfterStreamLoss();", added)
        self.assertIn("    renderControlResetProcess();", added)
        self.assertIn("    __atomic_store_n(&gSeqno, 0, __ATOMIC_SEQ_CST);", added)
        self.assertIn("dsa::StreamGenerations::beginNew();", text)
        # Each thread whose stream ended connects again, keeping the lost stream while the
        # renderer does not answer, and the instance waits for it.
        self.assertIn("GfxStreamConnectionManager::replaceThreadLocalInstance(", text)
        self.assertIn("dsa::shouldReconnect(", text)
        self.assertIn("        return VK_ERROR_INITIALIZATION_FAILED;", added)

    def test_nfr5_a_failed_reconnect_keeps_the_old_connection(self):
        added, _ = file_hunks(MESA_PATCH, "src/gfxstream/guest/connection-manager/GfxStreamConnectionManager.cpp")
        text = "\n".join(added)
        self.assertLess(text.index("if (!fresh->initialize()) {"), text.index("sThreadInfo.mgr = std::move(fresh);"))
        self.assertIn("        oldStream->decRef();", added)

    def test_nfr5_the_process_registers_again_with_a_restarted_renderer(self):
        added, _ = file_hunks(GFXSTREAM_PATCH, "guest/renderControl_enc/GfxStreamRenderControl.cpp")
        self.assertIn("void renderControlResetProcess() {", added)
        self.assertIn("    sNeedInit = true;", added)
        # A renderer that does not answer yet no longer ends the process through a null stream.
        self.assertIn("        if (!sProcessStream) {", added)
        added, _ = file_hunks(GFXSTREAM_PATCH, "guest/renderControl_enc/GfxStreamRenderControl.h")
        self.assertIn("void renderControlResetProcess();", added)

    def test_nfr5_the_driver_uses_the_tested_policy(self):
        added, _ = file_hunks(MESA_PATCH, "src/gfxstream/guest/vulkan_enc/VulkanStreamGuest.h")
        self.assertIn('#include "dsa_lost_stream.h"', added)
        self.assertIn("class LostStream {", HEADER.read_text(encoding="utf-8"))

    def test_nfr5_fetch_applies_the_guest_patches_after_the_ones_they_build_on(self):
        script = FETCH.read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/gfxstream/*.patch; do', script)
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', script)
        self.assertLess(script.index('gfxstream_patches=("$patch_dir"/0001-*.patch)'),
                        script.index('"$patch_dir"/guest/gfxstream/*.patch'))
        self.assertLess(script.index('"$patch_dir"/0008-*.patch'), script.index('"$patch_dir"/guest/mesa/*.patch'))


if __name__ == "__main__":
    unittest.main()
