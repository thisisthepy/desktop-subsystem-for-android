"""Tests for host patch 0048 and the renderer's list of the guests' allocations.

The renderer counted each guest device's memory by kind, but nothing said which allocation was
which: the dev.37 report found 30 MiB of CPU-mapped memory on the guests' devices with no reason
logged, and two of the ways host-visible memory missed the shared graphics memory were silent.
Patch 0048 has the decoder note why each guest allocation is what it is, and say those two
once; the renderer's memory hook keeps the note, the memory type and what the memory is
dedicated to, and lists every allocation for the memory snapshot. An allocation with no note
was made by the renderer itself on the guest's device."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
INCLUDE = HERE / "include"
HOST = HERE / "host"
WINDOWS = HERE.parent / "host" / "platform" / "windows" / "src"


def read(path):
    return path.read_text(encoding="utf-8")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


class AllocationReasonsPatchTests(unittest.TestCase):
    def setUp(self):
        self.patch = read(next(PATCHES.glob("0048-*.patch")))
        self.body = added(self.patch)

    def test_nfr1_both_silent_paths_are_said_once_and_noted(self):
        self.assertIn('dsaSayCopied(4, "the guest never said the device maps the shared graphics '
                      'memory");', self.body)
        self.assertIn("dsa::noteAllocation(dsa::AllocationWhy::DeviceNotShared);", self.body)
        self.assertIn("dsaSayCopied(5, \"a gfxstream feature", self.body)
        self.assertIn("dsa::noteAllocation(dsa::AllocationWhy::FeatureAllocates);", self.body)
        # The early return that said nothing is gone.
        self.assertIn("-            if (!deviceInfo || !deviceInfo->dsaMapsSharedMemory) return "
                      "std::nullopt;", self.patch)

    def test_nfr1_every_refusal_and_the_grant_itself_are_noted(self):
        for why in ("DedicatedImage", "MoreThanImportGives", "NoHostImport", "ImportAlignment",
                    "RegionRefused", "TypeRefused", "ImportRefused", "Granted", "Imported",
                    "GpuOnly"):
            self.assertIn(f"dsa::noteAllocation(dsa::AllocationWhy::{why});", self.body, why)

    def test_nfr1_the_grant_is_noted_just_before_its_import(self):
        # The import is a context line of the patch, so it is looked up in the whole patch.
        granted = self.patch.index("dsa::noteAllocation(dsa::AllocationWhy::Granted);")
        imported = self.patch.index("if (vk->vkAllocateMemory(device, &info, pAllocator, pMemory)")
        self.assertLess(granted, imported)

    def test_nfr1_the_hook_takes_the_note_and_lists_each_guest_allocation(self):
        memory = read(HOST / "dsa_gfx_memory.cpp")
        self.assertIn("const dsa::AllocationWhy why = dsa::takeAllocationNote();", memory)
        # Taken before the driver is called, so a failed call leaves no note behind.
        self.assertLess(memory.index("dsa::takeAllocationNote();"),
                        memory.index("VkResult result = entries.allocate("))
        self.assertIn("allocation.dedicated = dedicated_to(info);", memory)
        self.assertIn("dsa_gfx_describe_guest_allocations(char* out, size_t capacity)", memory)
        self.assertIn("dsa::allocationColumns(record)", memory)
        header = read(HOST / "dsa_gfx_backend.h")
        self.assertIn("DSA_GFX_EXPORT size_t dsa_gfx_describe_guest_allocations(", header)

    def test_nfr1_the_list_reaches_the_memory_snapshot_file(self):
        adapter = read(WINDOWS / "compositor" / "adapter.rs")
        self.assertIn('b"dsa_gfx_describe_guest_allocations\\0"', adapter)
        self.assertIn("color_buffers_with_allocations(colors, allocations)", adapter)

    def test_nfr1_the_note_header_has_its_test(self):
        self.assertIn("inline AllocationWhy takeAllocationNote()", read(INCLUDE / "dsa_allocation_note.h"))
        self.assertIn("allocation_note_test.cpp", read(HERE / "tests" / "test.sh"))

    def test_pr3_every_context_line_starts_with_a_space(self):
        in_hunk = False
        for line in self.patch.splitlines():
            if line.startswith("@@"):
                in_hunk = True
            elif line.startswith("diff --git"):
                in_hunk = False
            elif in_hunk:
                self.assertTrue(line[:1] in (" ", "+", "-"), repr(line))

    def test_pr3_the_fetch_applies_0048_after_0047(self):
        self.assertIn(" 0047 0048", read(HOST / "fetch.sh"))
        fetch = read(HOST / "fetch.ps1")
        self.assertLess(fetch.index('"0047-*.patch"'), fetch.index('"0048-*.patch"'))


if __name__ == "__main__":
    unittest.main()
