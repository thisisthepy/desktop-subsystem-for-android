"""Tests for shrinking the renderer's per-thread buffers after a large guest transfer.

Every guest thread with a graphics stream has a render thread on the host, and two buffers of
it grow with the largest thing that thread ever sent and never shrank (RESEARCH R33 on
develop: candidates for the gap between the renderer's private and resident memory):

- the render thread's ReadBuffer (host/ReadBuffer.cpp), which a batch of commands is read
  into; it grows to twice the largest batch, and a guest's copy of host-visible memory
  travels in one;
- the Vulkan decoder's BumpPool (aemu base/BumpPool.h), which a command's arguments are
  decoded into; it grows to twice the most one command needed.

Patch 0027 gives the ReadBuffer back its first size once it holds no data after a batch that
fits that size and no larger batch has come for 2 s, the rule of the Vulkan reply buffer
(include/dsa_reply_buffer.h, bufferShrinks). Releasing a buffer above 8 MiB as soon as it was
empty freed and made it again on every frame of a video's large uploads. aemu patch 0003 does the same for a
BumpPool above 1 MiB after 256 commands that needed a quarter of it or less."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
READ_BUFFER = next((HERE / "patches").glob("0027-*.patch"))
BUMP_POOL = next((HERE / "patches" / "aemu").glob("0003-*.patch"))


def added(patch, name):
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            return "\n".join(l[1:] for l in part.splitlines()
                             if l.startswith("+") and not l.startswith("+++"))
    raise AssertionError(f"{patch.name} does not change {name}")


class ReadBufferPatchTests(unittest.TestCase):
    def setUp(self):
        self.source = added(READ_BUFFER, "host/ReadBuffer.cpp")
        self.header = added(READ_BUFFER, "host/ReadBuffer.h")

    def test_nfr1_an_empty_read_buffer_goes_back_to_its_first_size(self):
        consume = self.source[self.source.index("    if (m_validData == 0 && m_shrinkDue) {"):]
        self.assertIn("shrinkWhenEmpty();", consume)
        shrink = self.source[self.source.index("void ReadBuffer::shrinkWhenEmpty() {"):]
        shrink = shrink[:shrink.index("\n}")]
        self.assertIn("if (m_validData == 0 && m_shrinkDue) {", self.source)
        self.assertIn("malloc(m_initialSize)", shrink)
        self.assertIn("free(m_buf);", shrink)
        self.assertIn("m_size = m_initialSize;", shrink)
        self.assertIn("m_readPtr = m_buf;", shrink)

    def test_nfr1_large_reads_that_keep_coming_keep_the_buffer(self):
        # No release at once for a large buffer: a video's uploads would free and make it on
        # every frame. The reply buffer's rule decides, with the first size as the size kept.
        self.assertNotIn("kReleaseAtOnce", self.header + self.source)
        self.assertIn('#include "dsa_reply_buffer.h"', self.source)
        self.assertIn("m_shrinkDue = dsa::bufferShrinks(minSize, m_size, m_initialSize, "
                      "dsa::ReplyClock::now(),", self.source)
        self.assertIn("m_initialSize = bufsize;", self.source)
        self.assertNotIn('#include "dsa_reply_buffer.h"', self.header)
        self.assertIn("std::chrono::steady_clock::time_point m_lastLargeRead{};", self.header)
        test = (HERE / "tests/reply_buffer_test.cpp").read_text(encoding="utf-8")
        self.assertIn("nfr1_steady_large_reads_keep_the_read_buffer();", test)
        self.assertIn("for (int frame = 1; frame <= 600; ++frame) {", test)
        # Both backends that may build ReadBuffer.cpp find the rule's header.
        cmake = (HERE / "host/CMakeLists.txt").read_text(encoding="utf-8")
        static = cmake[cmake.index("if(TARGET gfxstream_backend_static)"):]
        self.assertIn('"${CMAKE_CURRENT_SOURCE_DIR}/../include"', static[:static.index("endif()")])

    def test_nfr1_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0027", script.read_text(encoding="utf-8"))


class VulkanReplyBufferPatchTests(unittest.TestCase):
    """Patch 0028: the Vulkan decoder's reply buffer (host/vulkan/VulkanStream.cpp), which grew
    to twice the largest reply, a guest's copy of host-visible memory among them, goes back to
    256 KiB on a small reply once no large one has gone out for 2 s. The rule is in
    include/dsa_reply_buffer.h, tested by tests/reply_buffer_test.cpp."""

    def setUp(self):
        self.patch = next((HERE / "patches").glob("0028-*.patch"))
        self.source = added(self.patch, "host/vulkan/VulkanStream.cpp")
        self.header = added(self.patch, "host/vulkan/VulkanStream.h")

    def test_nfr1_each_commit_asks_whether_the_reply_buffer_goes_back(self):
        self.assertIn('#include "dsa_reply_buffer.h"', self.source)
        self.assertIn("const size_t sent = mWritePos;", self.source)
        self.assertIn("dsa::replyBufferShrinks(sent, mWriteBuffer.capacity(), dsa::ReplyClock::now(),",
                      self.source)
        self.assertIn("dsa::shrinkReplyBuffer(mWriteBuffer);", self.source)
        # The header other libraries include stays free of the folder the rule is in.
        self.assertNotIn('#include "dsa_reply_buffer.h"', self.header)
        self.assertIn("std::chrono::steady_clock::time_point mLastLargeWrite{};", self.header)
        self.assertIn("bool mSeenLargeWrite = false;", self.header)

    def test_nfr1_the_rule_keeps_large_steady_replies_and_is_tested(self):
        rule = (HERE / "include/dsa_reply_buffer.h").read_text(encoding="utf-8")
        self.assertIn("kReplyKeepBytes = 256 * 1024;", rule)
        self.assertIn("kReplyQuietFor{2000};", rule)
        self.assertIn('"$here/reply_buffer_test.cpp"',
                      (HERE / "tests/test.sh").read_text(encoding="utf-8"))
        # The decoder that compiles VulkanStream.cpp has the folder on its include path.
        cmake = (HERE / "host/CMakeLists.txt").read_text(encoding="utf-8")
        server = cmake[cmake.index("if(TARGET gfxstream-vulkan-server)"):]
        self.assertIn('"${CMAKE_CURRENT_SOURCE_DIR}/../include"', server[:server.index("endif()")])

    def test_nfr1_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0028", script.read_text(encoding="utf-8"))


class BumpPoolPatchTests(unittest.TestCase):
    def test_nfr1_a_bump_pool_that_grew_for_one_command_shrinks(self):
        text = added(BUMP_POOL, "base/include/aemu/base/BumpPool.h")
        self.assertIn("mTotalWantedThisGeneration * 4 < mStorage.size() * sizeof(uint64_t)", text)
        self.assertIn("if (++mSmallGenerations >= kSmallGenerationsBeforeShrink) {", text)
        self.assertIn("mStorage.resize(keep / sizeof(uint64_t));", text)
        self.assertIn("static constexpr size_t kShrinkAbove = 1024 * 1024;", text)

    def test_nfr1_both_fetches_apply_every_aemu_patch(self):
        self.assertIn('Join-Path $PatchDir "aemu"', (HERE / "host/fetch.ps1").read_text(
            encoding="utf-8"))
        self.assertIn('"$patch_dir"/aemu/*.patch', (HERE / "host/fetch.sh").read_text(
            encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
