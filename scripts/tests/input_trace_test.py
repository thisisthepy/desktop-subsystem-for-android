"""Tests for scripts/input-trace.py, which lays out a click's input trace on the host clock."""

import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
spec = importlib.util.spec_from_file_location("input_trace", HERE.parent / "input-trace.py")
trace = importlib.util.module_from_spec(spec)
spec.loader.exec_module(trace)

HOST_LOG = """\
[+0d 06:23:02.000 UTC] input-trace stage=app.pointer task=7 utc=2026-10-26T06:23:03.000Z mono=10.000 host=5000.000 mouse_down=true
[+0d 06:23:02.000 UTC] input-trace stage=wm.frame task=7 utc=2026-10-26T06:23:03.040Z mono=50.000 host=5040.000 sequence=3 round_trip=0.8
[+0d 06:23:02.000 UTC] input-trace stage=app.present task=7 utc=2026-10-26T06:23:03.041Z mono=51.000 host=5041.000 sequence=3 took=0.5
[+0d 06:23:02.000 UTC] input-trace stage=app.shown task=7 utc=2026-10-26T06:23:03.070Z mono=80.000 host=5070.000 present=12 vblank=5057.000 exact=true
[+0d 06:23:02.000 UTC] input-trace stage=app.pointer task=7 utc=2026-10-26T06:23:03.200Z mono=210.000 host=5200.000 mouse_down=false
[+0d 06:23:02.000 UTC] input-trace stage=app.present task=8 utc=2026-10-26T06:23:03.041Z mono=51.000 host=5010.000 sequence=9 took=0.5
"""

# The guest clock is 3000 ms behind the host's: guest = host - 3000, so offset = -3000.
SERVICE_LOG = """\
S-1-5-21: input-trace stage=clock task=0 utc=2026-10-26T06:23:02.500Z mono=1.000 host=4500.400 host_sent=4500.000 guest=1500.700 host_received=4500.400 rtt=0.400 offset=-2999.500
S-1-5-21: input-trace stage=clock task=0 utc=2026-10-26T06:23:03.500Z mono=2.000 host=5500.100 host_sent=5500.000 guest=2500.050 host_received=5500.100 rtt=0.100 offset=-3000.000
"""

GUEST_LOG = """\
[  12.3] I dsa-agent: input-trace stage=agent.inject task=7 action=POINTER_ACTION_DOWN utc=2026-10-26T06:23:03.003Z mono=2003.000 queued=0.2 inject=0.9
[  12.4] I dsa-agent: input-trace stage=app.frame pkg=com.android.settings window=0 utc=2026-10-26T06:23:03.010Z mono=2010.000 late=1.0 main=2.0 render=3.0 total=8.0 flags=0
[  12.4] I dsa-agent: input-trace stage=sf.frame pkg=com.android.settings layer=com.android.settings/Settings#1 utc=2026-10-26T06:23:03.010Z mono=2010.000 desired=2010.000 ready=2018.000 actual=2033.000
[  12.4] I hwc: input-trace stage=hwc.frame task=7 utc=2026-10-26T06:23:03.035Z mono=2035.000 waited=0.1 compose=2.0 post=0.3
"""


class InputTraceTest(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        root = Path(self.tmp.name)
        (root / "dsa-app-window.log").write_text(HOST_LOG)
        (root / "dsa-hostd.log").write_text(SERVICE_LOG)
        (root / "android-S-1-5-21.log").write_text(GUEST_LOG)
        self.root = root

    def tearDown(self):
        self.tmp.cleanup()

    def merged(self):
        lines = trace.read(trace.log_files([str(self.root)]))
        clock = trace.Clock(lines)
        return trace.events(trace.place(lines, clock), 1500.0)

    def test_fr5_a_trace_line_is_found_after_whatever_the_log_put_before_it(self):
        line = trace.parse_line("[  12.4] I hwc: input-trace stage=hwc.frame task=7 mono=1.5 post=0.3")
        self.assertEqual(line.stage, "hwc.frame")
        self.assertEqual(line.fields, {"task": "7", "mono": "1.5", "post": "0.3"})
        self.assertIsNone(trace.parse_line("nothing to see"))

    def test_fr5_guest_times_use_the_pairing_with_the_smallest_round_trip(self):
        lines = [trace.parse_line(t) for t in SERVICE_LOG.splitlines()]
        clock = trace.Clock(lines)
        guest = trace.parse_line(GUEST_LOG.splitlines()[3])
        self.assertAlmostEqual(clock.to_host(guest, 2035.0), 5035.0)

    def test_fr5_a_pairing_from_far_away_in_time_is_not_used(self):
        lines = [trace.parse_line(t) for t in SERVICE_LOG.splitlines()]
        clock = trace.Clock(lines)
        far = trace.parse_line(
            "input-trace stage=hwc.frame task=7 utc=2026-10-26T07:23:03.035Z mono=2035.000"
        )
        self.assertIsNone(clock.to_host(far, 2035.0))

    def test_fr5_each_press_and_release_lists_its_stages_in_order_on_the_host_clock(self):
        events = self.merged()
        self.assertEqual([e.press for e in events], [True, False])
        press = events[0]
        self.assertEqual(
            [line.stage for line in press.stages],
            [
                "app.pointer",
                "agent.inject",
                "app.frame",
                "sf.frame",
                "hwc.frame",
                "wm.frame",
                "app.present",
                "app.shown",
            ],
        )
        times = [round(line.host, 3) for line in press.stages]
        self.assertEqual(times, [5000.0, 5003.0, 5010.0, 5033.0, 5035.0, 5040.0, 5041.0, 5057.0])
        # Another task's present is not this click's.
        self.assertNotIn("sequence=9", trace.report(events))

    def test_fr5_the_report_gives_each_stages_gap_to_the_one_before(self):
        text = trace.report(self.merged())
        self.assertIn("press task=7 host=5000.000", text)
        self.assertIn("gap    23.0  sf.frame", text)
        self.assertIn("gap    16.0  app.shown", text)
        self.assertIn("main=2.0 render=3.0", text)
        self.assertIn("release task=7 host=5200.000", text)

    def test_fr5_without_arguments_it_reads_both_dsa_log_folders(self):
        old = dict(os.environ)
        try:
            os.environ["LOCALAPPDATA"] = r"C:\Users\me\AppData\Local"
            os.environ["ProgramData"] = r"C:\ProgramData"
            paths = trace.default_paths()
        finally:
            os.environ.clear()
            os.environ.update(old)
        self.assertEqual(len(paths), 2)
        self.assertTrue(paths[0].startswith(r"C:\Users\me\AppData\Local"))
        self.assertTrue(paths[1].startswith(r"C:\ProgramData"))


if __name__ == "__main__":
    unittest.main()
