"""Tests for the framework patches that show Google Play the computer's memory.

Play uploads ActivityManager.MemoryInfo.totalMem and advertisedMem. totalMem is the computer's
memory for them (0002, in libandroid_runtime). advertisedMem came from Process.getAdvertisedMem,
which rounds the total memory up to the next power of two in decimal units, so a 32 GB computer
(33.8 GB of memory) was advertised as 64 GB (0008, in services.jar: the image keeps the published
framework.jar, so the change cannot be in Process)."""

import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
BASE = HERE / "patches/frameworks/base"
VERSIONS = ("android16", "android17")
PROCESS_LIST = "services/core/java/com/android/server/am/ProcessList.java"


def patch(android, number):
    found = list((BASE / android).glob(f"{number}-*.patch"))
    assert len(found) == 1, (android, number)
    return found[0].read_text(encoding="utf-8").replace("\r\n", "\n")


def added(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("+") and not l.startswith("+++"))


def removed(text):
    return "\n".join(l[1:] for l in text.splitlines() if l.startswith("-") and not l.startswith("---"))


class ComputersMemoryPatchTests(unittest.TestCase):
    def test_fr32_advertised_mem_is_answered_in_services_jar_not_in_process(self):
        # framework.jar stays the published one, so a Process method the patch declared would be
        # missing at run time, and a native libandroid_runtime registers for it aborts zygote.
        for android in VERSIONS:
            text = patch(android, "0008")
            headers = [l for l in text.splitlines() if l.startswith(("--- ", "+++ "))]
            self.assertEqual(headers, [f"--- a/{PROCESS_LIST}", f"+++ b/{PROCESS_LIST}"], android)
            self.assertNotIn("getHostAdvertisedMem", text, android)
            self.assertNotIn("native", added(text), android)

    def test_fr32_advertised_mem_is_answered_for_the_listed_apps_only(self):
        for android in VERSIONS:
            text = patch(android, "0008")
            joined = added(text)
            # getMemoryInfo, which ActivityManager.getMemoryInfo reaches through binder, asks for
            # the caller's figure first and keeps Android's own value otherwise.
            self.assertIn("        outInfo.advertisedMem = getAdvertisedMem();", removed(text), android)
            self.assertIn("final long dsaAdvertisedMem = dsaHostAdvertisedMemForCaller();", joined, android)
            self.assertIn("outInfo.advertisedMem = dsaAdvertisedMem > 0 ? dsaAdvertisedMem : getAdvertisedMem();",
                          joined, android)
            # The callers are chosen as 0002 chooses the ones whose totalMem is the computer's:
            # the binder caller's app id, an app in uids and not in real_uids, never system_server.
            self.assertIn("final int caller = Binder.getCallingUid();", joined, android)
            self.assertIn("if (caller == Process.myUid()) {", joined, android)
            self.assertIn("final int app = UserHandle.getAppId(caller);", joined, android)
            self.assertIn('if (app < Process.FIRST_APPLICATION_UID || dsaHostMemoryListed("real_uids", app)',
                          joined, android)
            self.assertIn('|| !dsaHostMemoryListed("uids", app)) {', joined, android)
            self.assertIn('"/sys/module/dsa_host_memory/parameters/"', joined, android)
            self.assertIn('dsaHostMemoryRead(name).trim().split(",")', joined, android)

    def test_fr32_advertised_mem_is_the_installed_memory_in_whole_gib_counted_in_decimal_units(self):
        for android in VERSIONS:
            joined = added(patch(android, "0008"))
            self.assertIn('SystemProperties.get("ro.boot.dsa.host_installed_kb", "")', joined, android)
            # Rounded to the nearest GiB (a 31.5 GiB total gives 32), counted as ro.boot.ddr_size
            # is (32 GiB gives 32,000,000,000, not 64 GB and not 34,359,738,368), at least 1 GiB.
            self.assertIn("DSA_GIB_IN_KB = 1024L * 1024L;", joined, android)
            self.assertIn("(kb + DSA_GIB_IN_KB / 2) / DSA_GIB_IN_KB", joined, android)
            self.assertIn("return Math.max(gib, 1) * 1_000_000_000L;", joined, android)
            self.assertNotIn("roundStorageSize", joined, android)

    def test_fr32_advertised_mem_follows_the_computers_memory_when_the_host_sent_no_installed_figure(self):
        for android in VERSIONS:
            joined = added(patch(android, "0008"))
            self.assertIn("if (kb == 0) {\n            kb = dsaParseKb(dsaHostMemoryRead(\"store_total_kb\"));",
                          joined, android)

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
