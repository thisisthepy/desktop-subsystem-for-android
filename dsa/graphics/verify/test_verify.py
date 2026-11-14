"""Tests for the graphics check and measurement script: the parts that need no device."""

import importlib.util
import unittest
from pathlib import Path

SPEC = importlib.util.spec_from_file_location("verify", Path(__file__).with_name("verify.py"))
verify = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify)


class SubsetTests(unittest.TestCase):
    def test_pr3_the_subset_file_names_only_dEQP_VK_groups(self):
        patterns = verify.read_patterns(verify.SUBSET.read_text(encoding="utf-8"))
        self.assertTrue(patterns)
        files = verify.mustpass_files(patterns)
        self.assertIn("wsi.txt", files)
        self.assertIn("api.txt", files)

    def test_pr3_subset_comments_and_blank_lines_are_ignored(self):
        text = "# heading\n\ndEQP-VK.info.*  # all of it\n  dEQP-VK.api.smoke.triangle\n"
        self.assertEqual(verify.read_patterns(text), ["dEQP-VK.info.*", "dEQP-VK.api.smoke.triangle"])

    def test_pr3_a_pattern_without_a_group_is_rejected(self):
        with self.assertRaises(ValueError):
            verify.mustpass_files(["dEQP-VK.*"])
        with self.assertRaises(ValueError):
            verify.mustpass_files(["dEQP-GLES2.info.vendor"])

    def test_pr3_lavapipe_exceptions_are_not_patterns_and_keep_their_reason(self):
        text = ("dEQP-VK.api.smoke.*\n"
                "lavapipe: dEQP-VK.api.smoke.triangle  # Lavapipe draws it one pixel off\n")
        patterns, exceptions = verify.read_subset(text)
        self.assertEqual(patterns, ["dEQP-VK.api.smoke.*"])
        self.assertEqual(exceptions, {"dEQP-VK.api.smoke.triangle": "Lavapipe draws it one pixel off"})
        self.assertEqual(verify.excused("dEQP-VK.api.smoke.triangle", exceptions), "Lavapipe draws it one pixel off")
        self.assertIsNone(verify.excused("dEQP-VK.api.smoke.create_shader", exceptions))

    def test_pr3_a_lavapipe_exception_without_a_reason_is_refused(self):
        with self.assertRaises(ValueError):
            verify.read_subset("lavapipe: dEQP-VK.api.smoke.triangle\n")
        with self.assertRaises(ValueError):
            verify.read_subset("lavapipe:   # a reason for nothing\n")

    def test_pr3_every_lavapipe_exception_in_the_subset_file_has_a_reason(self):
        _, exceptions = verify.read_subset(verify.SUBSET.read_text(encoding="utf-8"))
        for pattern, reason in exceptions.items():
            self.assertTrue(pattern.startswith("dEQP-VK."), pattern)
            self.assertTrue(reason, pattern)

    def test_pr3_a_pattern_that_matches_no_case_is_reported(self):
        cases = ["dEQP-VK.info.build", "dEQP-VK.api.smoke.triangle", "dEQP-VK.api.smoke.create_shader"]
        selected, unmatched = verify.expand_subset(
            ["dEQP-VK.api.smoke.*", "dEQP-VK.info.renamed"], cases)
        self.assertEqual(selected, ["dEQP-VK.api.smoke.triangle", "dEQP-VK.api.smoke.create_shader"])
        self.assertEqual(unmatched, ["dEQP-VK.info.renamed"])


QPA = """#sessionInfo releaseName git-1
#beginSession
#beginTestCaseResult dEQP-VK.info.build
<TestCaseResult CasePath="dEQP-VK.info.build">
<Result StatusCode="Pass">Pass</Result>
</TestCaseResult>
#endTestCaseResult
#beginTestCaseResult dEQP-VK.wsi.android.surface.query_formats
<Result StatusCode="NotSupported">x</Result>
#endTestCaseResult
#beginTestCaseResult dEQP-VK.api.smoke.triangle
<Result StatusCode="Fail">Image comparison failed</Result>
#endTestCaseResult
#beginTestCaseResult dEQP-VK.memory.allocation.basic.size_4KiB.forward.count_1
#terminateTestCaseResult Timeout
#beginTestCaseResult dEQP-VK.draw.renderpass.simple_draw.simple_draw_triangle_list
"""


class QpaTests(unittest.TestCase):
    def test_pr3_cts_log_statuses_are_read_per_case(self):
        results = verify.parse_qpa(QPA)
        self.assertEqual(results.statuses["dEQP-VK.info.build"], "Pass")
        self.assertEqual(results.statuses["dEQP-VK.wsi.android.surface.query_formats"], "NotSupported")
        self.assertFalse(results.session_complete)

    def test_pr3_cts_failures_timeouts_and_crashes_fail_the_subset(self):
        failed = verify.parse_qpa(QPA).failed()
        self.assertEqual(failed, {
            "dEQP-VK.api.smoke.triangle": "Fail",
            "dEQP-VK.memory.allocation.basic.size_4KiB.forward.count_1": "Timeout",
            "dEQP-VK.draw.renderpass.simple_draw.simple_draw_triangle_list": "Crash",
        })

    def test_pr3_the_case_a_log_begins_but_never_ends_is_unfinished(self):
        self.assertEqual(verify.parse_qpa(QPA).unfinished,
                         "dEQP-VK.draw.renderpass.simple_draw.simple_draw_triangle_list")
        self.assertIsNone(verify.parse_qpa(QPA.rsplit("#beginTestCaseResult", 1)[0]).unfinished)

    def test_pr3_a_crash_fails_its_case_and_the_cases_after_it_still_run(self):
        cases = ["dEQP-VK.info.build", "dEQP-VK.api.smoke.triangle",
                 "dEQP-VK.api.smoke.asm_triangle", "dEQP-VK.synchronization.smoke.fences"]
        first = verify.parse_qpa(
            "#beginTestCaseResult dEQP-VK.info.build\n<Result StatusCode=\"Pass\">x</Result>\n"
            "#endTestCaseResult\n#beginTestCaseResult dEQP-VK.api.smoke.triangle\n")
        merged = verify.CtsResults()
        crashed = verify.merge_run(merged, first, cases, still_running=False)
        self.assertEqual(crashed, "dEQP-VK.api.smoke.triangle")
        left = verify.cases_left(cases, merged)
        self.assertEqual(left, cases[2:])
        second = verify.parse_qpa(
            "#beginTestCaseResult dEQP-VK.api.smoke.asm_triangle\n"
            "<Result StatusCode=\"Pass\">x</Result>\n#endTestCaseResult\n"
            "#beginTestCaseResult dEQP-VK.synchronization.smoke.fences\n"
            "<Result StatusCode=\"Pass\">x</Result>\n#endTestCaseResult\n#endSession\n")
        self.assertIsNone(verify.merge_run(merged, second, left, still_running=False))
        self.assertEqual(verify.cases_left(cases, merged), [])
        report = verify.cts_report(cases, merged, {}, False)
        self.assertEqual([entry["status"] for entry in report], ["Pass", "Crash", "Pass", "Pass"])
        self.assertEqual(verify.CtsResults(statuses=merged.statuses).failed(),
                         {"dEQP-VK.api.smoke.triangle": "Crash"})

    def test_pr3_a_case_still_running_when_the_time_runs_out_is_a_timeout(self):
        run = verify.parse_qpa("#beginTestCaseResult dEQP-VK.api.smoke.triangle\n")
        merged = verify.CtsResults()
        self.assertIsNone(verify.merge_run(merged, run, ["dEQP-VK.api.smoke.triangle"],
                                           still_running=True))
        self.assertEqual(merged.statuses, {"dEQP-VK.api.smoke.triangle": "Timeout"})

    def test_pr3_the_cts_process_is_looked_up_by_its_own_name(self):
        self.assertIn(f"{verify.DEQP_PACKAGE}:testercore", verify.DEQP_PROCESSES.split())

    def test_pr3_a_finished_session_is_recognised(self):
        self.assertTrue(verify.parse_qpa("#beginSession\n#endSession\n").session_complete)

    def test_pr3_the_report_has_each_case_and_a_case_that_never_ran_fails(self):
        cases = ["dEQP-VK.info.build", "dEQP-VK.wsi.android.surface.query_formats",
                 "dEQP-VK.api.smoke.triangle", "dEQP-VK.synchronization.smoke.fences"]
        report = verify.cts_report(cases, verify.parse_qpa(QPA), {}, False)
        self.assertEqual([entry["case"] for entry in report], cases)
        self.assertEqual([entry["status"] for entry in report], ["Pass", "NotSupported", "Fail", "NotRun"])
        self.assertEqual([entry["passed"] for entry in report], [True, True, False, False])
        lines = verify.report_lines(report)
        self.assertTrue(lines[0].startswith("PASS"))
        self.assertTrue(lines[2].startswith("FAIL"))
        self.assertTrue(lines[3].startswith("FAIL"))

    def test_pr3_a_lavapipe_exception_excuses_a_failure_only_on_software_vulkan(self):
        exceptions = {"dEQP-VK.api.smoke.*": "Lavapipe cannot"}
        cases = ["dEQP-VK.info.build", "dEQP-VK.api.smoke.triangle"]
        on_lavapipe = verify.cts_report(cases, verify.parse_qpa(QPA), exceptions, True)
        self.assertEqual(on_lavapipe[1]["excused"], "Lavapipe cannot")
        self.assertIsNone(on_lavapipe[0]["excused"])
        self.assertTrue(verify.report_lines(on_lavapipe)[1].startswith("EXCUSED"))
        on_a_gpu = verify.cts_report(cases, verify.parse_qpa(QPA), exceptions, False)
        self.assertIsNone(on_a_gpu[1]["excused"])

    def test_pr3_the_results_folder_holds_json_text_and_the_log(self):
        import json
        import tempfile

        with tempfile.TemporaryDirectory() as folder:
            out = Path(folder) / "results"
            log = Path(folder) / "vk-cts-results.qpa"
            log.write_text(QPA, encoding="utf-8")
            report = verify.cts_report(["dEQP-VK.info.build"], verify.parse_qpa(QPA), {}, True)
            verify.write_cts_results(out, report, {"ro.hardware.egl": "angle"}, [], log, "logcat")
            saved = json.loads((out / "vk-cts-results.json").read_text(encoding="utf-8"))
            self.assertEqual(saved["cases"][0]["status"], "Pass")
            self.assertIn("dEQP-VK.info.build", (out / "vk-cts-results.txt").read_text(encoding="utf-8"))
            self.assertTrue((out / "vk-cts-results.qpa").exists())
            self.assertEqual((out / "vk-cts-logcat.txt").read_text(encoding="utf-8"), "logcat")


class StackTests(unittest.TestCase):
    def test_pr3_every_app_must_use_angle_and_the_remoting_vulkan_driver(self):
        good = {"ro.hardware.egl": "angle", "ro.hardware.vulkan": "ranchu",
                "surfaceflinger": "GLES: Google Inc., ANGLE (Vulkan 1.3.0 (gfxstream)), OpenGL ES 3.2"}
        self.assertEqual(verify.check_stack(good), [])
        bad = dict(good, **{"ro.hardware.egl": "mesa", "surfaceflinger": "GLES: Mesa, llvmpipe"})
        self.assertEqual(len(verify.check_stack(bad)), 2)


class MeasurementTests(unittest.TestCase):
    def test_pr3_frame_times_come_from_presented_frames(self):
        pending = verify.INT64_MAX
        text = "16666666\n" + "\n".join([
            "1 1000000000 1",
            "1 1016666666 1",
            "1 1033333333 1",
            "1 1083333333 1",
            f"1 {pending} 1",
            "0 0 0",
        ])
        period, presents = verify.parse_sf_latency(text)
        self.assertEqual(period, 16666666)
        self.assertEqual(len(presents), 4)
        stats = verify.frame_stats(presents, period)
        self.assertEqual(stats["frames"], 4)
        self.assertEqual(stats["frame_ms_median"], 16.67)
        self.assertEqual(stats["frame_ms_max"], 50.0)
        self.assertEqual(stats["janky_frames"], 1)

    def test_pr3_no_frames_reports_zero_frames(self):
        self.assertEqual(verify.frame_stats([], 16666666), {"frames": 0})
        self.assertEqual(verify.parse_sf_latency(""), (None, []))

    def test_pr3_guest_cpu_is_busy_time_over_total_time(self):
        before = verify.parse_proc_stat("cpu  100 0 100 700 100 0 0 0 0 0\ncpu0 1 1 1 1\n")
        after = verify.parse_proc_stat("cpu  225 0 225 1350 200 0 0 0 0 0\n")
        self.assertEqual(verify.cpu_percent(before, after), 25.0)

    def test_pr3_guest_memory_in_use_is_total_less_available(self):
        text = "MemTotal:        4194304 kB\nMemFree:  100 kB\nMemAvailable:    3145728 kB\n"
        self.assertEqual(verify.parse_meminfo(text), 1024)

    def test_pr3_a_games_surface_view_is_measured_before_its_window(self):
        listing = ("com.android.systemui\n"
                   "com.android.hellovk/com.android.hellovk.VulkanActivity#12\n"
                   "SurfaceView[com.android.hellovk/com.android.hellovk.VulkanActivity]#13\n")
        layers = verify.pick_layers(listing, "com.android.hellovk")
        self.assertEqual(len(layers), 2)
        self.assertTrue(layers[0].startswith("SurfaceView"))


class AdbTests(unittest.TestCase):
    def test_fr20_the_guest_is_reached_through_developer_modes_loopback_port(self):
        self.assertEqual(verify.DEFAULT_SERIAL, "127.0.0.1:58526")
        self.assertEqual(verify.connect_command("127.0.0.1:58526"), ["adb", "connect", "127.0.0.1:58526"])
        self.assertIsNone(verify.connect_command("emulator-5554"))

    def test_pr3_the_adb_the_boot_checks_name_is_used(self):
        import os
        from unittest import mock

        with mock.patch.dict(os.environ, {"DSA_ADB": "/opt/platform-tools/adb"}):
            self.assertEqual(verify.adb_program(), "/opt/platform-tools/adb")
        with mock.patch.dict(os.environ, {}, clear=True):
            self.assertEqual(verify.adb_program(), "adb")


class RepositoryTests(unittest.TestCase):
    def test_pr3_the_test_apps_come_from_the_repository_named_with_repo(self):
        self.assertEqual(verify.repository("someone/dsa-fork", environ={}, origin_url=""), "someone/dsa-fork")

    def test_pr3_in_ci_the_test_apps_come_from_the_runs_own_repository(self):
        self.assertEqual(verify.repository(environ={"GITHUB_REPOSITORY": "someone/fork"}, origin_url=""), "someone/fork")

    def test_pr3_a_forks_clone_takes_the_test_apps_from_the_fork(self):
        for url in ("https://github.com/someone/desktop-subsystem-for-android.git",
                    "git@github.com:someone/desktop-subsystem-for-android.git",
                    "ssh://git@github.com/someone/desktop-subsystem-for-android",
                    "https://github.com/someone/desktop-subsystem-for-android\n"):
            self.assertEqual(verify.repository(environ={}, origin_url=url), "someone/desktop-subsystem-for-android", url)

    def test_pr3_without_a_github_origin_the_official_repository_is_used(self):
        self.assertEqual(verify.repository(environ={}, origin_url="https://example.com/x/y.git"),
                         verify.OFFICIAL_REPOSITORY)
        self.assertEqual(verify.repository(environ={}, origin_url=""), verify.OFFICIAL_REPOSITORY)


if __name__ == "__main__":
    unittest.main()
