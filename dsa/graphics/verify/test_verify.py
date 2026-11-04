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

    def test_pr3_a_finished_session_is_recognised(self):
        self.assertTrue(verify.parse_qpa("#beginSession\n#endSession\n").session_complete)


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


if __name__ == "__main__":
    unittest.main()
