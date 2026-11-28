"""Tests for the SurfaceFlinger patch that lets go of an idle virtual display's textures
(Android 16 0009, Android 17 0010).

The DSA agent draws each desktop widget in a window on a virtual display whose consumer is an
ImageReader in the agent. SurfaceFlinger kept a texture for each of the display's last buffers
for as long as the display existed, so the agent could not free them. Once nothing has been
composed into a virtual display for a second, RenderSurface now lets go of those textures, as
it does for an idle physical display's client target (0004). Whether the patch applies to the
pinned source, in order, is checked by .github/scripts/framework/check-patches.sh in the
framework build; these tests check what it changes."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
NATIVE = HERE / "patches/frameworks/native"
COMMITS = {"android16": "fcbde2bcff56ca1d7bec9cabf8bdca5e288bf103",
           "android17": "ae266dcb706d083868578cfedce381ef44488a07"}
# For each Android: this patch, and the one before it.
NUMBERS = {"android16": ("0009", "0008"), "android17": ("0010", "0009")}
SOURCE = "services/surfaceflinger/CompositionEngine/src/RenderSurface.cpp"
HEADER = "services/surfaceflinger/CompositionEngine/include/compositionengine/impl/RenderSurface.h"
AGENT_WIDGETS = HERE.parent / "agent/src/main/kotlin/io/github/thisisthepy/dsa/agent/platform/AndroidWidgets.kt"


def patch(android):
    found = list((NATIVE / android).glob(f"{NUMBERS[android][0]}-*.patch"))
    assert len(found) == 1, android
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def lines(text, marker, path=None):
    found = []
    current = None
    for line in text.splitlines():
        if line.startswith("+++ b/"):
            current = line[len("+++ b/"):]
        elif line.startswith("--- a/"):
            continue
        elif line.startswith(marker) and path in (None, current):
            found.append(line[1:])
    return found


def added(text, path=None):
    return "\n".join(lines(text, "+", path))


def hunks(text):
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


class IdleVirtualDisplayTests(unittest.TestCase):
    def test_nfr1_an_idle_virtual_display_lets_go_of_its_textures_after_a_second(self):
        for android in COMMITS:
            text = patch(android)
            self.assertIn("static constexpr nsecs_t kReleaseIdleVirtualTexturesAfter = 1'000'000'000; // 1 s",
                          added(text, HEADER), android)
            source = added(text, SOURCE)
            self.assertIn("} else if (mDisplay.isVirtual() && mTexture == nullptr && !mTextureCache.empty() &&",
                          source, android)
            self.assertIn("kReleaseIdleVirtualTexturesAfter) {", source, android)
            self.assertIn("        mTextureCache.clear();", source, android)

    def test_nfr1_each_frame_composed_into_the_display_counts_as_use(self):
        for android in COMMITS:
            source = added(patch(android), SOURCE)
            self.assertIn("    mLastClientCompositionTime = systemTime(SYSTEM_TIME_MONOTONIC);", source, android)

    def test_nfr1_a_physical_display_keeps_the_client_target_release_it_had(self):
        # The check comes after 0004's, as an else: a physical display goes on as before.
        for android in COMMITS:
            text = patch(android)
            self.assertEqual(lines(text, "-"), [], android)
            self.assertIn("\n         // The surface discarded its buffers; the textures kept for them are the last "
                          "holders.\n", text, android)

    def test_nfr1_the_release_logs_one_line(self):
        for android in COMMITS:
            self.assertIn('ALOGI("%s: nothing composed into the virtual display for %" PRId64',
                          added(patch(android), SOURCE), android)

    def test_nfr1_the_agent_gives_back_an_idle_widget_windows_free_buffers(self):
        # SurfaceFlinger lets go of its textures; the reader frees the buffers, which also makes
        # SurfaceFlinger's producer forget them.
        widgets = AGENT_WIDGETS.read_text(encoding="utf-8")
        self.assertIn("reader.discardFreeBuffers()", widgets)
        self.assertIn("idle.used()", widgets)
        self.assertIn("const val WINDOW_BUFFERS_IDLE_MILLIS = 5_000L", widgets)
        self.assertIn("idleBuffers.close()", widgets)

    def test_nfr1_every_hunk_counts_its_lines(self):
        for android in COMMITS:
            for start, old_count, new_count, body in hunks(patch(android)):
                for line in body:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, start, line))
                self.assertEqual(old_count, sum(1 for l in body if l[:1] in " -"), (android, start))
                self.assertEqual(new_count, sum(1 for l in body if l[:1] in " +"), (android, start))

    def test_nfr1_the_patch_comes_after_the_last_one_and_names_its_commit(self):
        for android, commit in COMMITS.items():
            number, before = NUMBERS[android]
            names = sorted(p.name[:4] for p in (NATIVE / android).glob("*.patch"))
            self.assertEqual(names, sorted(set(names)), android)
            self.assertEqual(names.index(number), names.index(before) + 1, android)
            self.assertIn(f"Applies to frameworks/native at {commit}", patch(android), android)
            self.assertIn(f"after {before}.", patch(android), android)

    def test_nfr1_no_em_dash(self):
        for android in COMMITS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
