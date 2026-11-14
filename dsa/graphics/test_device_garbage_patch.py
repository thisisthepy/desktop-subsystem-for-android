"""Tests for destroying a Vulkan device's garbage instead of leaking it.

On the owner's PC (Intel Arc, Windows 11, v0.2.3, 2026-11-12) the renderer logged
"DeviceOpTracker.cpp:126: VkDevice:... had a waitable pending for 21481 milliseconds. Leaking
object." 130 times and "DeviceOpTracker.cpp:161: VkDevice:... has 3 leaking garbage objects on
destruction." 112 times: every guest process (ANGLE makes each GL app a Vulkan client) that
destroyed its device left fences and semaphores with the driver.

The garbage is a fence or semaphore the guest destroyed while the operation that last used it
(a queue submission, an image acquire) still looked pending. An operation is seen to finish
only when its fence is polled, and the device's operations are polled only when the device is
used next. A guest waits for its fence and resets it before it submits again, so an operation
whose fence was reset before it was polled looked pending for ever, and everything waiting for
it was leaked after five seconds or when the device was destroyed, although the GPU had long
finished.

Patch 0024 polls the device's operations before a fence is reset, keeps overdue garbage for
the device's destruction instead of leaking it, and on destruction, once the device is idle,
destroys every object still held, and the fences operations made for themselves."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
PATCH = next((HERE / "patches").glob("0024-*.patch"))


def file_side(patch, name):
    """The added lines, and the lines the patch leaves (added and context), of one file."""
    text = patch.read_text(encoding="utf-8")
    for part in re.split(r"^diff --git ", text, flags=re.M):
        if part.startswith(f"a/{name} "):
            lines = part.splitlines()
            added = [l[1:] for l in lines if l.startswith("+") and not l.startswith("+++")]
            after = [l[1:] for l in lines
                     if (l.startswith("+") and not l.startswith("+++")) or l.startswith(" ")]
            return "\n".join(added), "\n".join(after)
    raise AssertionError(f"{patch.name} does not change {name}")


def body(text, signature):
    """The body of the function starting with `signature` in `text`, up to its closing brace."""
    start = text.index(signature)
    return text[start:text.index("\n}\n", start)]


class DeviceGarbagePatchTests(unittest.TestCase):
    def setUp(self):
        self.added, self.after = file_side(PATCH, "host/vulkan/DeviceOpTracker.cpp")

    def test_nfr2_overdue_garbage_is_kept_for_the_device_not_leaked(self):
        self.assertIn("mOverdueGarbage.push_back(pendingGarbage.obj);", self.added)
        self.assertNotIn("Leaking object", self.added)
        header, _ = file_side(PATCH, "host/vulkan/DeviceOpTracker.h")
        self.assertIn("std::deque<std::variant<VkFence, VkSemaphore>> mOverdueGarbage", header)

    def test_nfr2_destroying_a_device_destroys_all_it_still_holds(self):
        destroy = body(self.after, "void DeviceOpTracker::OnDestroyDevice() {")
        # Only after the device is idle.
        idle = destroy.index("mDeviceDispatch->vkDeviceWaitIdle(mDevice);")
        for step in ("stale.swap(mPollFunctions);",
                     "mDeviceDispatch->vkDestroyFence(mDevice, op.ownedFence, nullptr);",
                     "for (const PendingGarbage& pendingGarbage : mPendingGarbage) {",
                     "for (const auto& obj : mOverdueGarbage) {"):
            self.assertLess(idle, destroy.index(step), step)
        self.assertIn("mPendingGarbage.clear();", destroy)
        self.assertIn("mOverdueGarbage.clear();", destroy)
        self.assertNotIn("leaking garbage objects on destruction", destroy)

    def test_nfr2_an_operations_own_fence_is_known_to_the_tracker(self):
        self.assertIn("}, destroyFenceOnCompletion ? fence : VK_NULL_HANDLE);", self.added)
        self.assertIn(".ownedFence = ownedFence,", self.added)

    def test_nfr2_operations_are_polled_before_a_fence_is_reset(self):
        # In on_vkResetFences: after the fences to reset are known, before they are reset
        # (the block that calls vkResetFences starts with this test of cleanedFences).
        _, after = file_side(PATCH, "host/vulkan/VkDecoderGlobalState.cpp")
        known = after.index("mFenceInfo[pFences[i]].state = FenceInfo::State::kNotWaitable;")
        poll = after.index("deviceInfo->deviceOpTracker->PollAndProcessGarbage();")
        reset = after.index("        if (!cleanedFences.empty()) {", poll)
        self.assertLess(known, poll)
        self.assertLess(poll, reset)

    def test_nfr2_the_fetch_applies_it(self):
        for script in (HERE / "host/fetch.ps1", HERE / "host/fetch.sh"):
            self.assertIn("0024", script.read_text(encoding="utf-8"))


if __name__ == "__main__":
    unittest.main()
