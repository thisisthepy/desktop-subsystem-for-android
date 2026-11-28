"""Tests for the framework patch that stops the WebView zygote once no renderer has needed it for
a while (0016, in services.jar and framework.jar).

0005 starts the WebView zygote with the first WebView renderer rather than at boot; from then on
it ran until the guest stopped. The activity manager now checks it every minute once a renderer
started from it, and stops it when no process started from it has existed for three minutes.
WebViewZygote.getProcess starts it again for the next renderer. Whether the patch applies to the
pinned source, in order, is checked by .github/scripts/framework/check-patches.sh in the
framework build; these tests check what it changes."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
ZYGOTE = "core/java/android/webkit/WebViewZygote.java"
PROCESSES = "services/core/java/com/android/server/am/ProcessList.java"


def patch(android):
    found = list((BASE / android).glob("0016-*.patch"))
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


def method(joined, signature):
    start = joined.index(signature)
    depth = 0
    for i in range(start, len(joined)):
        if joined[i] == "{":
            depth += 1
        elif joined[i] == "}":
            depth -= 1
            if depth == 0:
                return joined[start:i + 1]
    raise AssertionError(f"{signature} does not end")


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


class WebViewZygotePatchTests(unittest.TestCase):
    def test_nfr1_the_zygote_stops_after_three_minutes_without_a_renderer(self):
        for android in VERSIONS:
            text = added(patch(android), PROCESSES)
            self.assertIn("private static final long DSA_WEBVIEW_ZYGOTE_IDLE_MS = 3 * 60 * 1000;", text, android)
            self.assertIn("private static final long DSA_WEBVIEW_ZYGOTE_CHECK_MS = 60 * 1000;", text, android)
            check = method(text, "private void dsaCheckWebViewZygote() {")
            self.assertIn("if (now - mDsaWebViewRendererSeen < DSA_WEBVIEW_ZYGOTE_IDLE_MS) {", check, android)
            self.assertLess(check.index("mDsaWebViewZygoteCheckPending = false;"),
                            check.index("WebViewZygote.stopIdleZygote()"), android)

    def test_nfr1_a_renderer_that_exists_or_is_starting_keeps_the_zygote(self):
        for android in VERSIONS:
            text = added(patch(android), PROCESSES)
            renderer = method(text, "private boolean dsaHasWebViewRendererLocked() {")
            self.assertIn("mProcessNames.getMap()", renderer, android)
            self.assertIn("hosting != null && hosting.usesWebviewZygote()", renderer, android)
            check = method(text, "private void dsaCheckWebViewZygote() {")
            self.assertIn("if (dsaHasWebViewRendererLocked()) {\n                mDsaWebViewRendererSeen = now;", check,
                          android)

    def test_nfr1_checks_start_with_a_renderer_and_never_race_one(self):
        for android in VERSIONS:
            text = added(patch(android), PROCESSES)
            # Each renderer start notes the time and starts the checks, before the zygote is asked.
            self.assertIn("            if (hostingRecord.usesWebviewZygote()) {", patch(android), android)
            self.assertIn("                dsaWebViewRendererStarting();", text, android)
            starting = method(text, "private void dsaWebViewRendererStarting() {")
            self.assertIn("mService.mProcStartHandler.postDelayed(this::dsaCheckWebViewZygote,", starting, android)
            # The check holds the activity manager's lock, as every start of a renderer does or
            # runs on the same handler.
            check = method(text, "private void dsaCheckWebViewZygote() {")
            self.assertTrue(check.split("\n")[1].strip() == "synchronized (mService) {", android)

    def test_nfr1_the_zygote_keeps_its_package_and_starts_again_for_the_next_renderer(self):
        for android in VERSIONS:
            stop = method(added(patch(android), ZYGOTE), "public static boolean stopIdleZygote() {")
            self.assertIn("synchronized (sLock) {", stop, android)
            self.assertIn("stopZygoteLocked();", stop, android)
            self.assertNotIn("sPackage", stop, android)
            # getProcess, which starts it when there is none, is as it was.
            self.assertEqual(lines(patch(android), "-"), [], android)

    def test_nfr1_a_stop_and_a_start_each_log_one_line(self):
        for android in VERSIONS:
            self.assertIn('Slog.i(TAG, "Stopped the WebView zygote: no WebView renderer for "',
                          added(patch(android), PROCESSES), android)
            self.assertIn('Log.i(LOGTAG, "Started the WebView zygote in " + (SystemClock.uptimeMillis() - start)',
                          added(patch(android), ZYGOTE), android)

    def test_nfr1_both_versions_make_the_same_change(self):
        for path in (ZYGOTE, PROCESSES):
            self.assertEqual(added(patch("android16"), path), added(patch("android17"), path), path)

    def test_nfr1_every_hunk_counts_its_lines(self):
        for android in VERSIONS:
            for start, old_count, new_count, body in hunks(patch(android)):
                for line in body:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, start, line))
                self.assertEqual(old_count, sum(1 for l in body if l[:1] in " -"), (android, start))
                self.assertEqual(new_count, sum(1 for l in body if l[:1] in " +"), (android, start))

    def test_nfr1_the_patch_comes_after_the_last_one_and_names_its_commit(self):
        for android, commit in COMMITS.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)
            self.assertIn("after 0015.", patch(android), android)

    def test_nfr1_no_em_dash(self):
        for android in VERSIONS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
