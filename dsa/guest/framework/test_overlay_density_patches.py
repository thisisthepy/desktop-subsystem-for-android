"""Tests for the framework patch that draws an app overlay window of an app set to forced system
scaling at the density of the host monitor it is on (0012, in framework.jar and services.jar).

The DSA agent gives one overlay window a density through the window manager's shell command
dsa-overlay-density. The window manager tells that window's process with
IWindow.executeCommand, and the process converts that window's views to the density in place with
0010's conversion and lays the window out again; the app's other windows keep their density.
Whether the patch applies to the pinned source, after 0011, is checked by
.github/scripts/framework/check-patches.sh in the framework build; these tests check what it
changes and that it changes nothing for a window that was not given a density."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
RESCALE = "core/java/android/view/DsaDensityRescale.java"
VIEW = "core/java/android/view/View.java"
ROOT = "core/java/android/view/ViewRootImpl.java"
IMAGE = "core/java/android/widget/ImageView.java"
TEXT = "core/java/android/widget/TextView.java"
SHELL = "services/core/java/com/android/server/wm/WindowManagerShellCommand.java"
STATE = "services/core/java/com/android/server/wm/WindowState.java"
FILES = [RESCALE, VIEW, ROOT, IMAGE, TEXT, SHELL, STATE]


def patch(android):
    found = list((BASE / android).glob("0012-*.patch"))
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


class OverlayDensityPatchTests(unittest.TestCase):
    def test_fr41_the_patch_changes_the_window_manager_and_the_apps_process(self):
        # services.jar takes the agent's word and passes it on; framework.jar, in the app's
        # process, converts the one window.
        for android in VERSIONS:
            headers = [l for l in patch(android).splitlines() if l.startswith(("--- ", "+++ "))]
            expected = [h for path in FILES for h in (f"--- a/{path}", f"+++ b/{path}")]
            self.assertEqual(headers, expected, android)

    def test_fr41_the_shell_command_answers_reads_and_gives_a_density(self):
        for android in VERSIONS:
            shell = added(patch(android), SHELL)
            self.assertIn('case "dsa-overlay-density":', shell, android)
            body = method(shell, "private int runDsaOverlayDensity(PrintWriter pw) {")
            self.assertIn("android.Manifest.permission.MANAGE_ACTIVITY_TASKS", body, android)
            self.assertIn('pw.println("supported");', body, android)
            # The window is named as SurfaceFlinger's list names it: its identity hash.
            self.assertIn("windowArg.equals(Integer.toHexString(System.identityHashCode(w)))",
                          body, android)
            # Without a density it says the one it was last given, for an agent that restarted.
            self.assertIn("pw.println(window.mDsaDensityDpi);", body, android)
            self.assertIn("window.mDsaDensityDpi = dpi;", body, android)
            self.assertIn("if (dpi != 0 && (dpi < 72 || dpi > 960)) {", body, android)

    def test_fr41_only_an_apps_overlay_window_is_given_a_density(self):
        for android in VERSIONS:
            shell = added(patch(android), SHELL)
            body = method(shell, "private int runDsaOverlayDensity(PrintWriter pw) {")
            self.assertIn("android.view.WindowManager.LayoutParams.isSystemAlertWindowType(", body,
                          android)
            self.assertIn("UserHandle.getAppId(window.getOwningUid())\n"
                          "                            < android.os.Process.FIRST_APPLICATION_UID",
                          body, android)
            # The app's process checks the type again before it takes the density.
            root = added(patch(android), ROOT)
            take = method(root, "void dsaSetWindowDensity(int densityDpi) {")
            self.assertIn("!WindowManager.LayoutParams.isSystemAlertWindowType(mWindowAttributes.type)",
                          take, android)

    def test_fr41_the_window_manager_tells_the_one_windows_process(self):
        for android in VERSIONS:
            shell = added(patch(android), SHELL)
            body = method(shell, "private int runDsaOverlayDensity(PrintWriter pw) {")
            self.assertIn("window.mClient.executeCommand(\n"
                          "                            android.view.DsaDensityRescale.WINDOW_DENSITY_COMMAND,\n"
                          "                            Integer.toString(dpi), pipe[1]);", body, android)
            self.assertIn('public static final String WINDOW_DENSITY_COMMAND = "DSA_WINDOW_DENSITY";',
                          added(patch(android), RESCALE), android)
            root = added(patch(android), ROOT)
            # Behind the DUMP check executeCommand already makes, and before any other command.
            self.assertIn("if (DsaDensityRescale.WINDOW_DENSITY_COMMAND.equals(command)) {\n"
                          "                        viewAncestor.dsaWindowDensityCommand(parameters, out);\n"
                          "                        return;", root, android)
            command = method(root, "void dsaWindowDensityCommand(String parameters, ParcelFileDescriptor out) {")
            # Taken on the window's own thread, never on the binder thread.
            self.assertIn("mHandler.post(() -> dsaSetWindowDensity(densityDpi));", command, android)
            self.assertIn("int mDsaDensityDpi;", added(patch(android), STATE), android)

    def test_fr41_the_windows_views_are_converted_in_place_and_laid_out_again(self):
        for android in VERSIONS:
            rescale = added(patch(android), RESCALE)
            body = method(rescale, "static void rescaleWindow(ViewRootImpl root) {")
            for step in ("sActive = true;", "rescaleTree(view);", "relayoutTree(view);",
                         "root.dsaWindowSizeChanged();", "view.requestApplyInsets();",
                         "view.requestLayout();"):
                self.assertIn(step, body, (android, step))
            root = added(patch(android), ROOT)
            changed = method(root, "void dsaWindowSizeChanged() {")
            self.assertIn("mWindowAttributesChanged = true;", changed, android)
            self.assertIn("mForceNextWindowRelayout = true;", changed, android)

    def test_fr41_a_view_takes_its_windows_density_only_in_such_a_window(self):
        for android in VERSIONS:
            text = patch(android)
            view = added(text, VIEW)
            target = method(view, "public final int dsaTargetDensity() {")
            self.assertIn("if (root != null && root.mDsaDensityDpi != 0) {\n"
                          "            return root.mDsaDensityDpi;", target, android)
            self.assertIn("return mResources != null ? mResources.getDisplayMetrics().densityDpi : 0;",
                          target, android)
            # 0010's check now converts to that density; nothing else of it changes.
            self.assertEqual(removed(text, VIEW),
                             ["        final int to = mResources.getDisplayMetrics().densityDpi;"],
                             android)
            self.assertIn("        final int to = dsaTargetDensity();", view, android)
            # A view that joins such a window takes its density, once it knows the window.
            attach = text.split("        mAttachInfo = info;\n", 1)[1]
            self.assertTrue(attach.startswith(
                "+        // DSA: a view that joins a window the DSA host gave a density of its own (an app\n"
                "+        // overlay window on a monitor of another density) takes that density now.\n"
                "+        dsaRescaleIfActive();\n"), android)

    def test_fr41_the_window_takes_the_size_its_app_gave_it_at_its_density(self):
        for android in VERSIONS:
            root = added(patch(android), ROOT)
            self.assertIn("            mDsaAppWidth = attrs.width;\n"
                          "            mDsaAppHeight = attrs.height;\n"
                          "            if (mDsaDensityDpi != 0) {\n"
                          "                dsaApplyWindowSize();", root, android)
            size = method(root, "private void dsaApplyWindowSize() {")
            # A size from the content (WRAP_CONTENT, MATCH_PARENT) is left alone.
            self.assertIn("mWindowAttributes.width = mDsaAppWidth > 0\n"
                          "                ? DsaDensityRescale.scale(mDsaAppWidth, from, to) : mDsaAppWidth;",
                          size, android)

    def test_fr41_what_the_app_sets_later_is_at_the_windows_density(self):
        for android in VERSIONS:
            text = patch(android)
            self.assertIn("if (unit != TypedValue.COMPLEX_UNIT_PX) {\n"
                          "            px = dsaToTargetDensity(px);", added(text, TEXT), android)
            self.assertIn("        dsaRetargetForWindow(d);", added(text, IMAGE), android)
            view = added(text, VIEW)
            self.assertIn("        dsaRetargetForWindow(background);", view, android)
            self.assertIn("        dsaRetargetForWindow(foreground);", view, android)

    def test_fr41_nothing_changes_in_a_process_no_overlay_was_given_a_density(self):
        for android in VERSIONS:
            view = added(patch(android), VIEW)
            # Every new check in a view stands behind 0010's flag, which only a converted
            # window sets.
            for name in ("public final float dsaToTargetDensity(float px) {",
                         "public final void dsaRetargetForWindow(Drawable drawable) {"):
                self.assertIn("!DsaDensityRescale.sActive", method(view, name), (android, name))
            self.assertNotIn("sActive = true", view, android)
            self.assertNotIn("sActive = true", added(patch(android), ROOT), android)
            # Nothing is taken out of these files: only added to.
            for path in (ROOT, IMAGE, RESCALE, SHELL, STATE):
                self.assertEqual(removed(patch(android), path), [], (android, path))

    def test_fr41_both_versions_make_the_same_change(self):
        for path in FILES:
            self.assertEqual(added(patch("android16"), path), added(patch("android17"), path), path)

    def test_fr41_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android).splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_fr41_the_patch_names_the_commit_it_was_made_for(self):
        for android, commit in COMMITS.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)
            self.assertIn("after 0011", patch(android), android)

    def test_fr41_no_em_dash(self):
        for android in VERSIONS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
