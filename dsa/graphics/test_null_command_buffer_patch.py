"""Tests for the guest Vulkan driver's handling of a command recorded into no command buffer.

YouTube, maximised to a 4090x2205 task, ended on a SIGSEGV in its GrallocUploadTh thread at
gfxstream_vk_CmdPipelineBarrier+117: `movq 0x1698(%rax)`, the command buffer's internal handle
read through a command buffer of 0. ANGLE had recorded an image's release to its foreign owner
into a primary command buffer it never allocated (nothing else was recorded since its last
submit, which a 64 MiB run of copies to images had made). Guest Mesa patch 0017 drops such a
command and says so once, and makes a failed vkAllocateCommandBuffers leave every handle
VK_NULL_HANDLE and nothing allocated, so an app never records into a command buffer that is not
there."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCHES = HERE / "patches"
PATCH = next((PATCHES / "guest/mesa").glob("0017-*.patch"))
TABLE = "src/gfxstream/guest/vulkan_enc/func_table.cpp"
COMMANDS = "src/gfxstream/guest/vulkan/gfxstream_vk_cmd.cpp"
PRIVATE = "src/gfxstream/guest/vulkan_enc/gfxstream_vk_private.h"
FROM_HANDLE = "VK_FROM_HANDLE(gfxstream_vk_command_buffer, gfxstream_commandBuffer, commandBuffer);"
# Every vkCmd entry point the pinned Mesa's func_table.cpp defines.
TABLE_COMMANDS = 131


def part(name):
    """The lines of the patch that change `name`."""
    text = PATCH.read_text(encoding="utf-8")
    for piece in re.split(r"^diff --git ", text, flags=re.M):
        if piece.startswith(f"a/{name} "):
            return piece.splitlines()
    raise AssertionError(f"{PATCH.name} does not change {name}")


def added(lines):
    return "\n".join(l[1:] for l in lines if l.startswith("+") and not l.startswith("+++"))


def guards(lines):
    """(trace scope, guarded name, line before) for each guard the patch adds."""
    found = []
    for i, line in enumerate(lines):
        match = re.match(
            r'^\+    if \(gfxstream_vk_drop_without_command_buffer\(gfxstream_commandBuffer, '
            r'"(vkCmd\w+)"\)\) \{$', line)
        if not match:
            continue
        scope = re.match(r'^     MESA_TRACE_SCOPE\("(vkCmd\w+)"\);$', lines[i - 2])
        found.append((scope.group(1) if scope else None, match.group(1), lines[i - 1][1:]))
        self_return = lines[i + 1:i + 3]
        assert self_return == ["+        return;", "+    }"], self_return
    return found


class NullCommandBufferPatchTests(unittest.TestCase):
    def test_nfr5_only_the_driver_files_change(self):
        files = re.findall(r"^diff --git a/(\S+) ", PATCH.read_text(encoding="utf-8"), flags=re.M)
        self.assertEqual(sorted(files), sorted([PRIVATE, COMMANDS, TABLE]))
        removed = [l for l in part(TABLE) if l.startswith("-") and not l.startswith("---")]
        self.assertEqual(removed, [], "the table only gains lines")

    def test_nfr5_every_vkcmd_entry_point_drops_a_command_without_a_command_buffer(self):
        table = guards(part(TABLE))
        self.assertEqual(len(table), TABLE_COMMANDS)
        commands = guards(part(COMMANDS))
        self.assertEqual([name for _, name, _ in commands], ["vkCmdBeginTransformFeedbackEXT"])
        names = set()
        for scope, name, before in table + commands:
            # Right after the handle is turned into the driver's object, before anything
            # reads through it.
            self.assertEqual(before.strip(), FROM_HANDLE, name)
            self.assertEqual(scope, name, "each guard names its own command")
            names.add(name)
        self.assertEqual(len(names), TABLE_COMMANDS + 1, "no command is guarded twice")
        self.assertIn("vkCmdPipelineBarrier", names)
        self.assertIn("vkCmdPipelineBarrier2", names)

    def test_nfr5_a_dropped_command_is_said_once_and_never_follows_the_handle(self):
        body = added(part(PRIVATE))
        self.assertIn("#include <atomic>", body)
        helper = body[body.index("static inline bool gfxstream_vk_drop_without_command_buffer("):]
        self.assertIn("if (commandBuffer) {\n        return false;", helper)
        self.assertIn("static std::atomic<bool> said{false};", helper)
        self.assertIn("if (!said.exchange(true)) {", helper)
        self.assertIn("mesa_logw(", helper)
        self.assertNotIn("abort", helper)
        self.assertNotIn("internal_object", helper)

    def test_nfr5_a_failed_command_buffer_allocation_leaves_no_handle_and_nothing_made(self):
        body = added(part(COMMANDS))
        self.assertIn("uint32_t created = 0;", body)
        self.assertIn("created++;", body)
        failure = body[body.index("if (VK_SUCCESS != result) {"):]
        self.assertIn("for (uint32_t i = 0; i < created; i++) {\n"
                      "            vk_command_buffer_destroyOp(&gfxstream_commandBuffers[i]->vk);",
                      failure)
        self.assertIn("pCommandBuffers[i] = VK_NULL_HANDLE;", failure)
        self.assertNotIn("result = VK_SUCCESS", failure, "the error reaches the caller")

    def test_nfr5_the_fetch_applies_it_after_0016(self):
        names = sorted(p.name for p in (PATCHES / "guest/mesa").glob("00*.patch"))
        self.assertEqual(names.index(PATCH.name),
                         names.index(next(n for n in names if n.startswith("0016-"))) + 1)
        fetch = (HERE / "fetch.sh").read_text(encoding="utf-8")
        self.assertIn('for patch in "$patch_dir"/guest/mesa/*.patch; do', fetch)


if __name__ == "__main__":
    unittest.main()
