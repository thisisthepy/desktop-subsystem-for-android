"""Tests for the framework patches that show Google Play the computer's memory.

Play uploads ActivityManager.MemoryInfo.totalMem and advertisedMem. totalMem is the computer's
memory for them (0002). advertisedMem came from Process.getAdvertisedMem, which rounds the total memory up
to the next power of two in decimal units, so a 32 GB computer (33.8 GB of memory) was advertised
as 64 GB (0008)."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")


def patch(android, number):
    found = list((BASE / android).glob(f"{number}-*.patch"))
    assert len(found) == 1, (android, number)
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


class ComputersMemoryPatchTests(unittest.TestCase):
    def test_fr32_advertised_mem_is_answered_for_the_listed_apps_only(self):
        for android in VERSIONS:
            text = patch(android, "0008")
            self.assertIn("+++ b/core/java/android/os/Process.java", text, android)
            self.assertIn("+++ b/core/jni/android_util_Process.cpp", text, android)
            joined = added(text)
            # The Java side asks the native one first and keeps Android's own value otherwise.
            self.assertIn("final long host = getHostAdvertisedMem();", joined, android)
            self.assertIn("if (host > 0) {", joined, android)
            # The native side reuses the uid lists of totalMem, so a caller that is not in uids,
            # or is in real_uids, gets 0.
            self.assertIn('!dsaHostMemoryListed("uids", app)', joined, android)
            self.assertIn('dsaHostMemoryListed("real_uids", app)', joined, android)
            self.assertIn('{"getHostAdvertisedMem", "()J"', joined, android)

    def test_fr32_advertised_mem_is_the_installed_memory_in_whole_gib_counted_in_decimal_units(self):
        for android in VERSIONS:
            joined = added(patch(android, "0008"))
            self.assertIn('"ro.boot.dsa.host_installed_kb"', joined, android)
            # Rounded to the nearest GiB (a 31.5 GiB total gives 32), counted as ro.boot.ddr_size
            # is (32 GiB gives 32,000,000,000, not 64 GB and not 34,359,738,368).
            self.assertIn("(kb + kGiBInKb / 2) / kGiBInKb", joined, android)
            self.assertIn("* 1000000000", joined, android)
            self.assertNotIn("roundStorageSize", joined, android)

    def test_fr32_advertised_mem_follows_the_computers_memory_when_the_host_sent_no_installed_figure(self):
        for android in VERSIONS:
            joined = added(patch(android, "0008"))
            self.assertIn('kb = dsaHostMemoryKb("store_total_kb");', joined, android)

    def test_fr32_every_hunk_line_starts_with_a_diff_marker(self):
        for android in VERSIONS:
            in_hunk = False
            for line in patch(android, "0008").splitlines():
                if line.startswith("@@"):
                    in_hunk = True
                elif line.startswith("diff --git"):
                    in_hunk = False
                elif in_hunk:
                    self.assertIn(line[:1], (" ", "+", "-"), (android, line))

    def test_fr32_the_patch_comes_after_the_one_it_builds_on(self):
        for android in VERSIONS:
            names = sorted(p.name[:4] for p in (BASE / android).glob("*.patch"))
            self.assertEqual(names, sorted(set(names)), android)
            self.assertLess(names.index("0002"), names.index("0008"), android)


if __name__ == "__main__":
    unittest.main()
