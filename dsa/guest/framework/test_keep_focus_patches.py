"""Tests for the framework patch that keeps the focus of an app set to keep game focus while
another app's window has it (0011, in framework.jar and services.jar).

The DSA agent sets the packages through the window manager's shell command dsa-keep-focus. Each
TopResumedActivityChangeItem then says whether its activity's package is one of them, and the
app's process does not report the loss of the top resumed state to such an activity while it
stays resumed, nor tell the views of its windows that the window lost the window focus while
another app's task is on top. The window focus itself, which decides where keys and IME text go,
follows the input dispatcher as before. Whether the patch applies to the pinned source, after
0010, is checked by .github/scripts/framework/check-patches.sh in the framework build; these
tests check what it changes and that it changes nothing for an app that is not set."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
THREAD = "core/java/android/app/ActivityThread.java"
ITEM = "core/java/android/app/servertransaction/TopResumedActivityChangeItem.java"
ROOT = "core/java/android/view/ViewRootImpl.java"
RECORD = "services/core/java/com/android/server/wm/ActivityRecord.java"
SHELL = "services/core/java/com/android/server/wm/WindowManagerShellCommand.java"
FILES = [THREAD, ITEM, ROOT, RECORD, SHELL]


def patch(android):
    found = list((BASE / android).glob("0011-*.patch"))
    assert len(found) == 1, android
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def lines(text, marker, path=None):
    """The lines of `path` (or every file) that start with `marker` ("+" added, "-" removed)."""
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


def removed(text, path=None):
    return lines(text, "-", path)


def method(joined, signature):
    """The body of the method that starts with `signature` among the added lines."""
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


class KeepFocusPatchTests(unittest.TestCase):
    def test_fr44_the_patch_changes_the_window_manager_and_the_apps_process(self):
        # services.jar sets which packages keep their focus and says so with each item;
        # framework.jar, in the app's process, leaves the losses out.
        for android in VERSIONS:
            headers = [l for l in patch(android).splitlines() if l.startswith(("--- ", "+++ "))]
            expected = [h for path in FILES for h in (f"--- a/{path}", f"+++ b/{path}")]
            self.assertEqual(headers, expected, android)

    def test_fr44_the_shell_command_answers_and_replaces_the_packages(self):
        for android in VERSIONS:
            shell = added(patch(android), SHELL)
            self.assertIn('case "dsa-keep-focus":', shell, android)
            body = method(shell, "private int runDsaKeepFocus(PrintWriter pw) {")
            self.assertIn("android.Manifest.permission.MANAGE_ACTIVITY_TASKS", body, android)
            self.assertIn('pw.println("supported");', body, android)
            self.assertIn('if (!"set".equals(verb)) {', body, android)
            # The whole set is replaced, so "set" alone switches every app off.
            self.assertIn("ActivityRecord.sDsaKeepFocusPackages = java.util.Collections.unmodifiableSet(packages);",
                          body, android)

    def test_fr44_every_app_is_off_until_the_agent_says_otherwise(self):
        for android in VERSIONS:
            record = added(patch(android), RECORD)
            self.assertIn("static volatile java.util.Set<String> sDsaKeepFocusPackages = "
                          "java.util.Collections.emptySet();", record, android)
            body = method(record, "boolean dsaKeepFocus() {")
            self.assertIn("return packageName != null && sDsaKeepFocusPackages.contains(packageName);",
                          body, android)

    def test_fr44_every_top_resumed_item_carries_the_mark(self):
        for android in VERSIONS:
            text = patch(android)
            record = added(text, RECORD)
            self.assertIn("new TopResumedActivityChangeItem(token, onTop,\n                dsaKeepFocus());",
                          record, android)
            self.assertEqual(
                removed(text, RECORD),
                ["        final TopResumedActivityChangeItem item = new TopResumedActivityChangeItem(token, onTop);"],
                android)
            item = added(text, ITEM)
            # Written and read in the same place, so both ends of the parcel agree.
            self.assertIn("dest.writeBoolean(mDsaKeepFocus);", item, android)
            self.assertIn("mDsaKeepFocus = in.readBoolean();", item, android)
            self.assertIn("r.mDsaKeepFocus = mDsaKeepFocus;", item, android)
            # The two-argument constructor stays, and means an app that is not set.
            self.assertIn("this(activityToken, onTop, false);", item, android)

    def test_fr44_a_kept_app_is_not_told_it_lost_the_top_while_it_stays_resumed(self):
        for android in VERSIONS:
            thread = added(patch(android), THREAD)
            guard = thread.index("            if (!onTop && r.mDsaKeepFocus) {")
            report = thread.index('            reportTopResumedActivityChanged(r, onTop, "topStateChangedWhenResumed");')
            self.assertLess(guard, report, android)
            # Only inside the resumed branch: the hunk's context is that branch's first line.
            hunk = patch(android).split("if (r.getLifecycleState() == ON_RESUME) {", 1)[1]
            self.assertTrue(hunk.lstrip("\n").startswith("+            // DSA:"), android)
            # The loss before onPause is Android's own and stays: nothing in the pause path changes.
            self.assertNotIn("performPauseActivityIfNeeded(", "\n".join(removed(patch(android), THREAD)), android)
            self.assertEqual(removed(patch(android), THREAD), [], android)

    def test_fr44_the_window_side_keeps_a_loss_only_for_a_marked_resumed_activity(self):
        for android in VERSIONS:
            thread = added(patch(android), THREAD)
            body = method(thread, "public int dsaWindowFocusKept(IBinder activityToken) {")
            self.assertIn("if (r == null || !r.mDsaKeepFocus || r.getLifecycleState() != ON_RESUME) {\n"
                          "            return DSA_FOCUS_NOT_KEPT;", body, android)
            self.assertIn("return r.isTopResumedActivity ? DSA_FOCUS_MAYBE_KEPT : DSA_FOCUS_KEPT;", body, android)

    def test_fr44_keys_and_ime_still_follow_the_window_focus(self):
        for android in VERSIONS:
            text = patch(android)
            # Only the two lines that tell the views are moved under the guard; the window's
            # own focus, the IME's and the input stages' are left as they are.
            self.assertEqual(removed(text, ROOT), [
                "            mView.dispatchWindowFocusChanged(hasWindowFocus);",
                "            mAttachInfo.mTreeObserver.dispatchOnWindowFocusChange(hasWindowFocus);",
            ], android)
            root = added(text, ROOT)
            for untouched in ("mAttachInfo.mHasWindowFocus = ", "mImeFocusController.",
                              "mFirstInputStage.onWindowFocusChanged", "handlePointerCaptureChanged"):
                self.assertNotIn(untouched, root, (android, untouched))
            self.assertIn("            if (fakeFocus || !dsaKeepsFocus(hasWindowFocus)) {", root, android)

    def test_fr44_a_hidden_window_hears_the_loss_it_was_spared(self):
        for android in VERSIONS:
            root = added(patch(android), ROOT)
            self.assertIn("            if (!visible) {\n                dsaReleaseFocusLoss();", root, android)
            release = method(root, "private void dsaReleaseFocusLoss() {")
            self.assertIn("mView.dispatchWindowFocusChanged(false);", release, android)
            keeps = method(root, "private boolean dsaKeepsFocus(boolean hasWindowFocus) {")
            # A window that is not visible keeps nothing.
            self.assertIn("if (!mAppVisible) {\n            return false;", keeps, android)

    def test_fr44_both_versions_make_the_same_change(self):
        for path in FILES:
            self.assertEqual(added(patch("android16"), path), added(patch("android17"), path), path)

    def test_fr44_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android).splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_fr44_the_patch_names_the_commit_it_was_made_for(self):
        for android, commit in COMMITS.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)
            self.assertIn("after 0010", patch(android), android)

    def test_fr44_no_em_dash(self):
        for android in VERSIONS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
