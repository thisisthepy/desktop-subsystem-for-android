"""Tests for the framework patch that sends a memory trim to an app the host hides although
Android shows its task (0013, in services.jar).

Android counts a task whose host window the user minimized or covered as visible, so its process
stays in the foreground and the activity manager's setProcessMemoryTrimLevel refuses it
TRIM_MEMORY_UI_HIDDEN and TRIM_MEMORY_BACKGROUND. The activity manager's shell command
dsa-hidden-trim takes the packages the DSA agent says the host hides ("hidden") and sends a
process of one of them a background level ("send"), for the shell only. Whether the patch
applies to the pinned source, after 0012, is checked by .github/scripts/framework/check-patches.sh
in the framework build; these tests check what it changes and that it changes nothing for
send-trim-memory or an app that is not set."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
COMMITS = {"android16": "45034f0663f960d9ee5fb0a101a4732b71f6e2f4",
           "android17": "94b4c163b7dfe5ce3607f7bb8456f9573f7de57d"}
SERVICE = "services/core/java/com/android/server/am/ActivityManagerService.java"
SHELL = "services/core/java/com/android/server/am/ActivityManagerShellCommand.java"
FILES = [SERVICE, SHELL]
# The lock each version's own setProcessMemoryTrimLevel holds.
LOCKS = {"android16": "this", "android17": "mGlobalLock"}


def patch(android):
    found = list((BASE / android).glob("0013-*.patch"))
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


class HiddenTrimPatchTests(unittest.TestCase):
    def test_nfr1_the_patch_changes_only_the_activity_manager(self):
        for android in VERSIONS:
            headers = [l for l in patch(android).splitlines() if l.startswith(("--- ", "+++ "))]
            expected = [h for path in FILES for h in (f"--- a/{path}", f"+++ b/{path}")]
            self.assertEqual(headers, expected, android)

    def test_nfr1_send_trim_memory_and_every_other_line_stay_as_in_aosp(self):
        # Nothing is removed, so setProcessMemoryTrimLevel, which a normal adb user's
        # send-trim-memory reaches, still refuses a background level to a foreground process.
        for android in VERSIONS:
            self.assertEqual(removed(patch(android)), [], android)
            service = added(patch(android), SERVICE)
            self.assertNotIn("setProcessMemoryTrimLevel(String", service, android)
            self.assertNotIn("PROCESS_STATE_IMPORTANT_FOREGROUND", service, android)

    def test_nfr1_the_shell_command_answers_supported(self):
        for android in VERSIONS:
            shell = added(patch(android), SHELL)
            self.assertIn('case "dsa-hidden-trim":\n                    return runDsaHiddenTrim(pw);', shell, android)
            body = method(shell, "int runDsaHiddenTrim(PrintWriter pw) throws RemoteException {")
            self.assertIn('if (verb == null) {\n            pw.println("supported");\n            return 0;', body, android)

    def test_nfr1_only_the_agent_sets_the_hidden_apps_and_the_set_is_replaced(self):
        for android in VERSIONS:
            body = method(added(patch(android), SHELL), "int runDsaHiddenTrim(PrintWriter pw) throws RemoteException {")
            hidden = body[body.index('if ("hidden".equals(verb)) {'):body.index('if (!"send".equals(verb)) {')]
            self.assertIn("enforceCallingOrSelfPermission(\n"
                          "                    android.Manifest.permission.MANAGE_ACTIVITY_TASKS", hidden, android)
            self.assertIn("mInternal.mDsaHiddenPackages = java.util.Collections.unmodifiableSet(packages);",
                          hidden, android)
            # Nothing is set until the agent sets it.
            self.assertIn("volatile java.util.Set<String> mDsaHiddenPackages = java.util.Collections.emptySet();",
                          added(patch(android), SERVICE), android)

    def test_nfr1_only_ui_hidden_and_background_are_sent_and_a_refusal_is_one_line(self):
        for android in VERSIONS:
            body = method(added(patch(android), SHELL), "int runDsaHiddenTrim(PrintWriter pw) throws RemoteException {")
            self.assertIn("level = ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN;", body, android)
            self.assertIn("level = ComponentCallbacks2.TRIM_MEMORY_BACKGROUND;", body, android)
            self.assertIn('getErrPrintWriter().println("Error: expected HIDDEN or BACKGROUND, not " + levelArg);',
                          body, android)
            self.assertIn("mInternal.dsaSetHiddenProcessMemoryTrimLevel(proc, UserHandle.USER_CURRENT, level);",
                          body, android)
            self.assertIn('} catch (IllegalArgumentException | SecurityException e) {', body, android)
            self.assertIn('getErrPrintWriter().println("Error: " + e.getMessage());\n            return -1;',
                          body, android)

    def test_nfr1_the_trim_goes_only_from_the_shell_to_an_app_the_host_hides(self):
        for android in VERSIONS:
            body = method(added(patch(android), SERVICE),
                          "boolean dsaSetHiddenProcessMemoryTrimLevel(String process, @UserIdInt int userId, int level)")
            shell_check = body.index('if (!isCallerShell()) {\n            throw new SecurityException("Only shell can call it");')
            level_check = body.index("if (level < ComponentCallbacks2.TRIM_MEMORY_UI_HIDDEN) {")
            hidden_check = body.index("if (app.info == null || !mDsaHiddenPackages.contains(app.info.packageName)) {")
            send = body.index("thread.scheduleTrimMemory(level);")
            self.assertLess(shell_check, level_check, android)
            self.assertLess(level_check, hidden_check, android)
            self.assertLess(hidden_check, send, android)
            # As setProcessMemoryTrimLevel: a level is sent once, never lower than the last.
            self.assertIn("if (app.mProfile.getTrimMemoryLevel() >= level) {", body, android)
            self.assertIn("app.mProfile.setTrimMemoryLevel(level);", body, android)
            self.assertIn(f"synchronized ({LOCKS[android]}) {{", body, android)

    def test_nfr1_the_trim_changes_nothing_about_the_task_or_its_windows(self):
        # Only the trim callback is sent: no change of process state, no stop, no visibility.
        for android in VERSIONS:
            body = method(added(patch(android), SERVICE),
                          "boolean dsaSetHiddenProcessMemoryTrimLevel(String process, @UserIdInt int userId, int level)")
            for untouched in ("setProcState", "setCurProcState", "updateOomAdj", "setVisibility",
                              "makeInvisible", "stopActivity", "mWindowManager", "mAtmInternal"):
                self.assertNotIn(untouched, body, (android, untouched))

    def test_nfr1_both_versions_make_the_same_change_but_for_the_lock(self):
        for path in FILES:
            sixteen = added(patch("android16"), path).replace("synchronized (this) {", "synchronized (LOCK) {")
            seventeen = added(patch("android17"), path).replace("synchronized (mGlobalLock) {",
                                                                "synchronized (LOCK) {")
            self.assertEqual(sixteen, seventeen, path)

    def test_nfr1_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android).splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_nfr1_the_patch_names_the_commit_it_was_made_for(self):
        for android, commit in COMMITS.items():
            self.assertIn(f"Applies to frameworks/base at {commit}", patch(android), android)
            self.assertIn("after 0012", patch(android), android)

    def test_nfr1_no_em_dash(self):
        for android in VERSIONS:
            self.assertNotIn(chr(0x2014), patch(android), android)


if __name__ == "__main__":
    unittest.main()
