"""Tests for the framework patch that gives a task marked for forced system scaling a new density
without relaunching its activities (0009, in services.jar).

The DSA agent marks the task through the window manager's shell command dsa-forced-density, and
ActivityRecord.shouldRelaunchLocked then treats a change of density as handled by the task's
activities. Whether the patch applies to the pinned source is checked by
.github/scripts/framework/check-patches.sh in the framework build; these tests check what it says
and that it decides the way the agent and the host rely on."""

import re
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
WM = "services/core/java/com/android/server/wm"
FILES = [f"{WM}/ActivityRecord.java", f"{WM}/Task.java", f"{WM}/WindowManagerShellCommand.java"]
# The variable shouldRelaunchLocked collects the handled changes in.
MASK = {"android16": "configChanged", "android17": "skipRelaunchConfigMask"}

# android.content.pm.ActivityInfo's values.
CONFIG_ORIENTATION = 0x0080
CONFIG_SCREEN_LAYOUT = 0x0100
CONFIG_UI_MODE = 0x0200
CONFIG_SCREEN_SIZE = 0x0400
CONFIG_SMALLEST_SCREEN_SIZE = 0x0800
CONFIG_DENSITY = 0x1000
CONFIG_LOCALE = 0x0004
CONSTANTS = {
    "ActivityInfo.CONFIG_DENSITY": CONFIG_DENSITY,
    "CONFIG_SCREEN_SIZE": CONFIG_SCREEN_SIZE,
    "CONFIG_SMALLEST_SCREEN_SIZE": CONFIG_SMALLEST_SCREEN_SIZE,
    "CONFIG_SCREEN_LAYOUT": CONFIG_SCREEN_LAYOUT,
}


def patch(android):
    found = list((BASE / android).glob("0009-*.patch"))
    assert len(found) == 1, android
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def added(text, path=None):
    lines = []
    current = None
    for line in text.splitlines():
        if line.startswith("+++ b/"):
            current = line[len("+++ b/"):]
        elif line.startswith("+") and not line.startswith("+++") and path in (None, current):
            lines.append(line[1:])
    return "\n".join(lines)


def density_changes(joined):
    """DSA_DENSITY_CHANGES as the patch defines it, from the constants it names."""
    match = re.search(r"static final int DSA_DENSITY_CHANGES = ([^;]+);", joined)
    assert match, "DSA_DENSITY_CHANGES is not defined"
    names = [name.strip() for name in match.group(1).split("|")]
    value = 0
    for name in names:
        value |= CONSTANTS[name]
    return value


def decide(joined, changes, forced):
    """The static dsaForcedDensityMask(changes, forced), run as the patch writes it: it returns
    0 unless the task is marked and the density changes, else DSA_DENSITY_CHANGES."""
    body = re.search(r"static int dsaForcedDensityMask\(int changes, boolean forced\) \{\n(.*?)\n    \}\n",
                     joined + "\n", re.S)
    assert body, "the decision function is missing"
    expected = ("        if (!forced || (changes & ActivityInfo.CONFIG_DENSITY) == 0) {\n"
                "            return 0;\n"
                "        }\n"
                "        return DSA_DENSITY_CHANGES;")
    assert body.group(1) == expected, body.group(1)
    if not forced or changes & CONFIG_DENSITY == 0:
        return 0
    return density_changes(joined)


def relaunches(joined, changes, handled, forced):
    """shouldRelaunchLocked's answer for an activity whose manifest handles `handled`."""
    return (changes & ~(handled | decide(joined, changes, forced))) != 0


class ForcedDensityPatchTests(unittest.TestCase):
    def test_fr4_the_patch_changes_the_window_manager_in_services_jar_only(self):
        # The window manager alone: the agent reaches it through a shell command of the window
        # manager's binder. What the app's process does with the new density is 0010's.
        for android in VERSIONS:
            headers = [l for l in patch(android).splitlines() if l.startswith(("--- ", "+++ "))]
            expected = [h for path in FILES for h in (f"--- a/{path}", f"+++ b/{path}")]
            self.assertEqual(headers, expected, android)

    def test_fr4_a_marked_task_takes_a_change_of_density_without_a_relaunch(self):
        density_move = CONFIG_DENSITY | CONFIG_SCREEN_SIZE | CONFIG_SMALLEST_SCREEN_SIZE | CONFIG_SCREEN_LAYOUT
        for android in VERSIONS:
            joined = added(patch(android))
            self.assertEqual(density_changes(joined), density_move, android)
            for changes in (CONFIG_DENSITY, CONFIG_DENSITY | CONFIG_SCREEN_SIZE, density_move):
                self.assertFalse(relaunches(joined, changes, 0, True), (android, hex(changes)))

    def test_fr4_an_unmarked_task_relaunches_as_before(self):
        for android in VERSIONS:
            joined = added(patch(android))
            for changes in (CONFIG_DENSITY, CONFIG_DENSITY | CONFIG_SCREEN_SIZE, CONFIG_SCREEN_SIZE):
                self.assertTrue(relaunches(joined, changes, 0, False), (android, hex(changes)))
            # An activity that declares density keeps doing so.
            self.assertFalse(relaunches(joined, CONFIG_DENSITY, CONFIG_DENSITY, False), android)

    def test_fr4_a_marked_task_still_relaunches_for_any_other_change(self):
        for android in VERSIONS:
            joined = added(patch(android))
            # A resize alone is no change of density, and keeps Android's own handling.
            self.assertTrue(relaunches(joined, CONFIG_SCREEN_SIZE, 0, True), android)
            for other in (CONFIG_ORIENTATION, CONFIG_UI_MODE, CONFIG_LOCALE):
                self.assertTrue(relaunches(joined, CONFIG_DENSITY | other, 0, True), (android, hex(other)))

    def test_fr4_should_relaunch_adds_the_mask_last(self):
        # Last, so that no app compat rule after it takes the density back out of the mask.
        for android in VERSIONS:
            text = patch(android)
            var = MASK[android]
            self.assertIn(f"+        {var} |= dsaForcedDensityMask(changes);\n+\n"
                          f"         return (changes & (~{var})) != 0;\n", text, android)

    def test_fr4_the_mark_is_a_field_of_the_task_read_on_the_leaf_and_its_root(self):
        for android in VERSIONS:
            text = patch(android)
            self.assertIn("    boolean mDsaForcedDensity;", added(text, f"{WM}/Task.java"), android)
            joined = added(text, f"{WM}/ActivityRecord.java")
            self.assertIn("if (leaf.mDsaForcedDensity) {", joined, android)
            self.assertIn("final Task root = leaf.getRootTask();", joined, android)
            self.assertIn("return root != null && root.mDsaForcedDensity;", joined, android)

    def test_fr4_the_shell_command_marks_a_live_task_for_a_caller_that_manages_tasks(self):
        for android in VERSIONS:
            joined = added(patch(android), f"{WM}/WindowManagerShellCommand.java")
            self.assertIn('case "dsa-forced-density":\n                    return runDsaForcedDensity(pw);', joined,
                          android)
            self.assertIn("android.Manifest.permission.MANAGE_ACTIVITY_TASKS", joined, android)
            # Without arguments it only answers, which is how the agent finds the patch.
            self.assertIn('pw.println("supported");', joined, android)
            # Only a task that exists now: no recent task is restored for it.
            self.assertIn("RootWindowContainer.MATCH_ATTACHED_TASK_ONLY", joined, android)
            self.assertIn("synchronized (mInternal.mGlobalLock) {", joined, android)
            self.assertIn('task.mDsaForcedDensity = "true".equals(valueArg);', joined, android)
            # The agent fails a command whose output starts with "Error".
            for line in re.findall(r'getErrPrintWriter\(\)\.println\("([^"]*)', joined):
                self.assertTrue(line.startswith("Error: "), (android, line))

    def test_fr4_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android).splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_fr4_the_patch_names_the_commit_it_was_made_for(self):
        commits = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
                   "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
        for android, commit in commits.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)


if __name__ == "__main__":
    unittest.main()
