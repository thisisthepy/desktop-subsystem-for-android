"""Tests for handing back the spare pages of the guest's reused transfer buffers: each thread's
stream to the host renderer (patches/guest/gfxstream/0002, on the stream of patches/0001), the
guest Vulkan driver's copy of what a CPU wrote (patches/guest/mesa/0011, on 0008) and the
hwcomposer's (dsa/guest/hwcomposer/src/cpu_upload.cpp). Each grew to its largest transfer and
kept it resident for the process's life. The rule is in include/dsa_spare_pages.h, tested by
tests/spare_pages_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
STREAM_PATCH = next((HERE / "patches/guest/gfxstream").glob("0002-*.patch"))
UPLOAD_PATCH = next((HERE / "patches/guest/mesa").glob("0011-*.patch"))
HEADER = HERE / "include/dsa_spare_pages.h"
TESTS = HERE / "tests/test.sh"
CMAKE = HERE / "cmake/CMakeLists.txt"
HWC_UPLOAD = HERE.parent / "guest/hwcomposer/src/cpu_upload.cpp"


def added(patch, name):
    """The added lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return [l[1:] for l in part.splitlines() if l.startswith("+") and not l.startswith("+++")]
    raise AssertionError(f"{patch.name} does not change {name}")


class SparePagesTests(unittest.TestCase):
    def test_nfr1_the_pages_go_back_lazily_and_the_warm_part_stays(self):
        header = HEADER.read_text(encoding="utf-8")
        # MADV_FREE, not MADV_DONTNEED: a transfer written before the kernel reclaims reuses the
        # pages without a fault, so a playing video pays nothing.
        self.assertIn("MADV_FREE", header)
        self.assertNotIn("MADV_DONTNEED", header)
        self.assertIn("kSpareKeepBytes = 256 * 1024", header)

    def test_nfr1_the_stream_hands_back_its_spare_pages_after_each_transfer(self):
        lines = added(STREAM_PATCH, "guest/OpenglSystemCommon/HvSocketStream.cpp")
        self.assertIn('#include "dsa_spare_pages.h"', lines)
        body = "\n".join(lines)
        self.assertLess(body.index("writeFully(buffer_, size)"),
                        body.index("dsa::releaseSparePages(buffer_, size)"))
        # Both guest builds of the stream find the header.
        self.assertIn("${CMAKE_CURRENT_SOURCE_DIR}/../include", CMAKE.read_text(encoding="utf-8"))
        self.assertIn('-I"$here/../include" -c', TESTS.read_text(encoding="utf-8"))

    def test_nfr1_both_upload_copies_hand_back_their_spare_pages_once_the_host_has_them(self):
        lines = added(UPLOAD_PATCH, "src/gfxstream/guest/android/GrallocMinigbm.cpp")
        self.assertIn('#include "dsa_spare_pages.h"', lines)
        self.assertIn("    dsa::releaseSparePages(mUploadStaging.data(), mUploadStaging.size());", lines)
        hwc = HWC_UPLOAD.read_text(encoding="utf-8")
        self.assertIn('#include "dsa_spare_pages.h"', hwc)
        self.assertLess(hwc.index("host.uploadColorBuffer(colorBuffer, layout, mStaging.data())"),
                        hwc.index("dsa::releaseSparePages(mStaging.data(), mStaging.size())"))

    def test_nfr1_the_rule_is_tested_on_the_build_machine(self):
        self.assertIn('"$here/spare_pages_test.cpp"', TESTS.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
