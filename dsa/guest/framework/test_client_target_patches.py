"""Tests for the SurfaceFlinger patches that free the client target's buffers when idle.

The client target is as large as the display (two buffers of 3840x3840 are 112.5 MiB). DSA's
hardware composer composes every layer itself, so SurfaceFlinger needs those buffers only while
a layer must be composed there. The first patches (Android 16 0004, Android 17 0005) free them
once nothing is composed there for a second, but only when the surface holds a buffer it
acquired, so a display that never used client composition kept both buffers the queue holds
from the time the display was set up. The later patches (Android 16 0007, Android 17 0008) free
those too."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
NATIVE = HERE / "patches/frameworks/native"
# For each Android: the class SurfaceFlinger uses for a physical display, the patch that adds
# the release, and the one that extends it to buffers never acquired.
VERSIONS = {
    "android16": ("FramebufferSurface", "0004", "0007"),
    "android17": ("LegacyFramebufferSurface", "0005", "0008"),
}


def patch(android, number):
    found = list((NATIVE / android).glob(f"{number}-*.patch"))
    assert len(found) == 1, (android, number)
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


def removed(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("-") and not l.startswith("---"))


def hunks(text):
    """Yields (old start, old count, new count, lines) for every hunk."""
    current = None
    for line in text.splitlines():
        if line.startswith("diff --git"):
            if current:
                yield current
            current = None
        elif line.startswith("@@"):
            if current:
                yield current
            old, new = line.split()[1:3]
            old_start, old_count = (int(x) for x in old[1:].split(","))
            _, new_count = (int(x) for x in new[1:].split(","))
            current = (old_start, old_count, new_count, [])
        elif current is not None:
            current[3].append(line)
    if current:
        yield current


class ClientTargetPatchTests(unittest.TestCase):
    def test_nfr1_buffers_never_composed_into_are_freed_when_idle(self):
        for android, (cls, _, later) in VERSIONS.items():
            text = patch(android, later)
            self.assertIn(f"DisplayHardware/{cls}.cpp", text, android)
            self.assertIn("mReleaseWhenCommitted = mCurrentBuffer != nullptr &&", removed(text), android)
            self.assertIn(
                "mReleaseWhenCommitted = (mCurrentBuffer != nullptr || mMayHoldFreeBuffers) &&",
                added(text),
                android,
            )

    def test_nfr1_the_queue_counts_as_holding_buffers_until_the_first_release(self):
        for android, (cls, _, later) in VERSIONS.items():
            self.assertIn("bool mMayHoldFreeBuffers = true;", added(patch(android, later)), android)

    def test_nfr1_an_idle_display_frees_its_buffers_once_not_on_every_frame(self):
        # Cleared when the buffers are freed, set again when a frame composes into the surface.
        for android, (_, _, later) in VERSIONS.items():
            lines = added(patch(android, later)).splitlines()
            self.assertIn("    mMayHoldFreeBuffers = false;", lines, android)
            self.assertIn("    mMayHoldFreeBuffers = true;", lines, android)

    def test_nfr1_the_release_still_waits_a_second_after_the_last_client_frame(self):
        for android, (_, _, later) in VERSIONS.items():
            self.assertIn(">= kReleaseIdleBuffersAfter;", patch(android, later), android)

    def test_nfr1_every_hunk_counts_its_lines(self):
        for android, (_, _, later) in VERSIONS.items():
            for start, old_count, new_count, lines in hunks(patch(android, later)):
                for line in lines:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, start, line))
                self.assertEqual(old_count, sum(1 for l in lines if l[:1] in " -"), (android, start))
                self.assertEqual(new_count, sum(1 for l in lines if l[:1] in " +"), (android, start))

    def test_nfr1_the_patch_comes_after_the_one_it_builds_on(self):
        for android, (_, first, later) in VERSIONS.items():
            names = sorted(p.name[:4] for p in (NATIVE / android).glob("*.patch"))
            self.assertEqual(names, sorted(set(names)), android)
            self.assertLess(names.index(first), names.index(later), android)


if __name__ == "__main__":
    unittest.main()
