"""Tests for reading host-visible memory back without sending what the device did not change.

A Hyper-V VM has no memory the guest and the host share, so the guest driver keeps a copy of
each mapped host-visible allocation and reads the device's writes back at each fence the app
sees signalled (guest/mesa 0005). Each read back sent every mapped byte, so every frame of
every app waited for its mapped memory to cross the socket. On the owner's PC with v0.2.1 and
v0.2.2, Google Play drew at about 7 frames a second (dumpsys gfxinfo: 150 ms median, almost
all of it issuing draw commands), and a resized window waited 0.8 to 3 s for a frame of its
new size, against 80 ms in v0.1.

The host renderer now records what the guest's copy holds and sends back only the 64 KiB
pieces the device changed (patches/0020, dsa_shadow_record.h). The guest asks in those
pieces, and takes block by block only what the device changed (guest/mesa 0010,
dsa_shadow_blocks.h). The C++ halves are tested in tests/shadow_record_test.cpp,
tests/shadow_blocks_test.cpp and tests/readback_latency_test.cpp."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
GUEST_PATCH = next((HERE / "patches/guest/mesa").glob("0010-*.patch"))
HOST_PATCH = next((HERE / "patches").glob("0020-*.patch"))
TRACKER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.cpp"
HEADER = "src/gfxstream/guest/vulkan_enc/ResourceTracker.h"
ENCODER = "src/gfxstream/guest/vulkan_enc/VkEncoder.cpp"
DECODER = "host/vulkan/VkDecoder.cpp"


def file_hunks(patch, name):
    """The added and removed lines of one file in a patch."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            return ([l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")],
                    [l[1:] for l in lines if l.startswith("-") and not l.startswith("---")])
    raise AssertionError(f"{patch.name} does not change {name}")


class ReadBackPatchTests(unittest.TestCase):
    def test_fr4_the_host_sends_back_only_what_the_device_changed(self):
        added, removed = file_hunks(HOST_PATCH, DECODER)
        self.assertFalse(removed)
        text = "\n".join(added)
        self.assertIn('#include "dsa_shadow_record.h"', text)
        # A flush records what the guest sent; a read back asks the record per range, and a
        # range nothing changed in answers with size 0; a freed allocation is forgotten.
        self.assertIn("dsa::shadowRecord().flushed((uint64_t)memory, mappedSize, offset,", text)
        self.assertIn("if (!dsa::shadowRecord().mustSend((uint64_t)memory, hostPtr, mappedSize,",
                      text)
        self.assertIn("vkStream->write(&writeStream, sizeof(uint64_t));", text)
        self.assertIn("dsa::shadowRecord().forget((uint64_t)memory);", text)

    def test_fr4_the_host_fetch_applies_it_and_the_decoder_finds_its_header(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0020", script.read_text(encoding="utf-8"))
        cmake = (HERE / "host/CMakeLists.txt").read_text(encoding="utf-8")
        self.assertIn('target_include_directories(gfxstream-vulkan-server PUBLIC\n'
                      '        "${CMAKE_CURRENT_SOURCE_DIR}/../include")', cmake)
        self.assertTrue((HERE / "include/dsa_shadow_record.h").is_file())

    def test_fr4_the_guest_asks_in_pieces_and_no_longer_takes_everything_as_the_hosts(self):
        added, removed = file_hunks(GUEST_PATCH, TRACKER)
        text = "\n".join(added)
        self.assertIn("            ranges.push_back({VK_STRUCTURE_TYPE_MAPPED_MEMORY_RANGE, nullptr, memory, 0,",
                      removed)
        self.assertIn("static constexpr VkDeviceSize kReadBackPiece = 64 * 1024;", text)
        self.assertNotIn("VK_WHOLE_SIZE", text)
        # The whole copy was marked as the host's after each read back, which lost what the
        # app wrote since its last submit once the host sends only what changed.
        self.assertIn("                it->second.shadowBlocks->refresh(it->second.ptr);", removed)
        self.assertNotIn("refresh(", text)
        self.assertIn("info.shadowBlocks->receive(info.ptr, offset, data, size)", text)
        self.assertIn('"input-trace stage=driver.readback reads=%u asked_mb_per_read=%.2f "', text)
        added, _ = file_hunks(GUEST_PATCH, HEADER)
        self.assertIn("    void receiveReadBack(VkDeviceMemory memory, uint64_t offset, const uint8_t* data,",
                      added)

    def test_fr4_the_encoder_hands_each_piece_it_reads_to_the_tracker(self):
        added, removed = file_hunks(GUEST_PATCH, ENCODER)
        self.assertIn("                if (copied) stream->read(hostPtr + offset, copied);", removed)
        text = "\n".join(added)
        self.assertIn("stream->read(piece.data(), length);", text)
        self.assertIn("sResourceTracker->receiveReadBack(memory, at, piece.data(), length);", text)

    def test_fr4_it_comes_after_the_patches_it_builds_on_and_its_tests_run(self):
        names = sorted(p.name for p in (HERE / "patches/guest/mesa").glob("*.patch"))
        # After 0007, whose first-map read back and kept-in-step test it uses.
        self.assertGreater(names.index(GUEST_PATCH.name),
                           names.index(next(n for n in names if n.startswith("0007-"))))
        tests = (HERE / "tests/test.sh").read_text(encoding="utf-8")
        for test in ("shadow_record_test.cpp", "readback_latency_test.cpp", "shadow_blocks_test.cpp"):
            self.assertIn(f'"$here/{test}"', tests)


if __name__ == "__main__":
    unittest.main()
