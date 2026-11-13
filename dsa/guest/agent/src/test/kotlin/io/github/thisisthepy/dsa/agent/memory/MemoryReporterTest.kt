package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.Agent
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.FakeEndpoint
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.FakeLocator
import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.FakeTasks
import io.github.thisisthepy.dsa.agent.FakeWindowing
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.RequestGuestMemory
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test

// A guest's /proc/meminfo, cut down, as a 6.18 kernel writes it.
private const val MEMINFO = """MemTotal:        4015356 kB
MemFree:          812444 kB
MemAvailable:    2311020 kB
Buffers:           10240 kB
Cached:          1503112 kB
SwapCached:          512 kB
Active:           900000 kB
AnonPages:        700400 kB
Shmem:             20480 kB
KReclaimable:      90000 kB
Slab:             160000 kB
SReclaimable:      80000 kB
SwapTotal:       2007676 kB
SwapFree:        1907676 kB
HugePages_Total:       0
"""

class FakeMemory : MemorySource {
    var meminfoText = MEMINFO
    var zram: String? = "104857600 26214400 31457280 0 31457280 0 0 0 0\n"
    var running = listOf(
        ProcessPss(500, "system", 180_000, 1_000),
        ProcessPss(900, "com.android.settings", 90_000),
        ProcessPss(901, "com.android.deskclock", 60_000),
    )
    var failProcesses = false
    var reporting: PageReporting? = null

    override fun meminfo() = meminfoText

    override fun zramMmStat() = zram

    override fun pageReporting() = reporting

    override fun processes(): List<ProcessPss> {
        if (failProcesses) throw SecurityException("no")
        return running
    }
}

class MemoryReporterTest {
    @Test
    fun nfr1_meminfo_totals_are_read_in_kib() {
        val info = MemInfo.parse(MEMINFO)
        assertEquals(4015356L, info.memTotalKb)
        assertEquals(812444L, info.memFreeKb)
        assertEquals(2311020L, info.memAvailableKb)
        assertEquals(10240L, info.buffersKb)
        assertEquals(1503112L, info.cachedKb)
        assertEquals(700400L, info.anonPagesKb)
        assertEquals(20480L, info.shmemKb)
        assertEquals(80000L, info.slabReclaimableKb)
        assertEquals(2007676L, info.swapTotalKb)
        assertEquals(1907676L, info.swapFreeKb)
    }

    @Test
    fun nfr1_the_report_breaks_down_what_the_kernel_holds() {
        val kernel = """SUnreclaim:        61440 kB
KernelStack:       10240 kB
PageTables:        20480 kB
VmallocUsed:       30720 kB
Percpu:             2048 kB
CmaTotal:          16384 kB
HugePages_Total:       2
Hugepagesize:       2048 kB
"""
        val report = guestMemory(MemInfo.parse(MEMINFO + kernel), null, emptyList(), memOnlineKb = 3_145_728)
        assertEquals(61440L, report.slabUnreclaimableKb)
        assertEquals(10240L, report.kernelStackKb)
        assertEquals(20480L, report.pageTablesKb)
        assertEquals(30720L, report.vmallocUsedKb)
        assertEquals(2048L, report.percpuKb)
        assertEquals(16384L, report.cmaTotalKb)
        assertEquals(4096L, report.hugetlbKb)
        assertEquals(3_145_728L, report.memOnlineKb)
    }

    @Test
    fun nfr1_meminfo_lines_it_cannot_read_are_skipped() {
        val info = MemInfo.parse("garbage\nMemTotal: lots kB\nMemFree: 12 kB\n:\n")
        assertEquals(0L, info.memTotalKb)
        assertEquals(12L, info.memFreeKb)
    }

    @Test
    fun nfr1_zram_mm_stat_gives_stored_and_used_kib() {
        val stat = ZramStat.parse("104857600 26214400 31457280 0 31457280 0 0 0 0\n")!!
        assertEquals(102400L, stat.originalKb)
        assertEquals(30720L, stat.usedKb)
        assertNull(ZramStat.parse("12 34"))
    }

    @Test
    fun nfr1_the_report_lists_the_largest_processes_first_at_most_ten() {
        val processes = (1..15).map { ProcessPss(it, "p$it", it * 1000L) }
        val report = guestMemory(MemInfo.parse(MEMINFO), null, processes)
        assertEquals(10, report.topProcessesCount)
        assertEquals(15, report.getTopProcesses(0).pid)
        assertEquals(6, report.getTopProcesses(9).pid)
        assertEquals((1..15).sumOf { it * 1000L }, report.totalPssKb)
        assertEquals(0L, report.zramUsedKb)
    }

    private val scheduler = FakeScheduler()
    private val log = RecordingLog()
    private val daemon = FakeEndpoint()
    private val locator = FakeLocator()
    private val memory = FakeMemory()

    private fun started(): Agent {
        locator.up = daemon
        val agent = Agent(
            AGENT, FakeTasks(), FakeActions(), locator, FakeWindowing(), FakeInjector(), scheduler, log,
            memory = memory,
        )
        agent.start()
        scheduler.runDue()
        return agent
    }

    private fun memoryReports() = daemon.sentMessages().filter { it.bodyCase == Envelope.BodyCase.GUEST_MEMORY }

    @Test
    fun nfr1_the_agent_reports_memory_when_the_host_asks() {
        started()
        daemon.deliver(Envelope.newBuilder().setRequestGuestMemory(RequestGuestMemory.getDefaultInstance()).build().toByteArray())
        scheduler.runDue()

        val reports = memoryReports()
        assertEquals(1, reports.size)
        val report = reports[0].guestMemory
        assertEquals(4015356L, report.memTotalKb)
        assertEquals(102400L, report.zramOriginalKb)
        assertEquals(30720L, report.zramUsedKb)
        assertEquals(330_000L, report.totalPssKb)
        assertEquals("system", report.getTopProcesses(0).name)
        assertEquals(1_000L, report.getTopProcesses(0).swapPssKb)
    }

    @Test
    fun nfr1_the_agent_reports_memory_periodically_while_connected() {
        started()
        assertEquals(0, memoryReports().size)
        scheduler.advance(MemoryReporter.FIRST_DELAY_MILLIS)
        assertEquals(1, memoryReports().size)
        scheduler.advance(MemoryReporter.INTERVAL_MILLIS)
        assertEquals(2, memoryReports().size)
    }

    @Test
    fun nfr2_the_report_says_what_free_page_reporting_gave_back_to_the_host() {
        memory.reporting = PageReporting(freeReportedKb = 700_000, reportedTotalKb = 9_000_000, refaultedTotalKb = 12_000)
        started()
        daemon.deliver(Envelope.newBuilder().setRequestGuestMemory(RequestGuestMemory.getDefaultInstance()).build().toByteArray())
        scheduler.runDue()

        val report = memoryReports().single().guestMemory
        assertEquals(700_000L, report.freeReportedKb)
        assertEquals(9_000_000L, report.reportedTotalKb)
        assertEquals(12_000L, report.refaultedTotalKb)
    }

    @Test
    fun nfr2_a_kernel_without_reporting_counts_sends_zeros_which_the_host_reads_as_unknown() {
        val report = guestMemory(MemInfo.parse(MEMINFO), null, emptyList())
        assertEquals(0L, report.freeReportedKb)
        assertEquals(0L, report.reportedTotalKb)
        assertEquals(0L, report.refaultedTotalKb)
    }

    @Test
    fun nfr1_a_failing_process_list_still_gives_the_kernel_totals() {
        memory.failProcesses = true
        memory.zram = null
        started()
        daemon.deliver(Envelope.newBuilder().setRequestGuestMemory(RequestGuestMemory.getDefaultInstance()).build().toByteArray())
        scheduler.runDue()

        val report = memoryReports().single().guestMemory
        assertEquals(4015356L, report.memTotalKb)
        assertEquals(0L, report.totalPssKb)
        assertEquals(0L, report.zramUsedKb)
        assertTrue(log.lines.any { it.startsWith("warn: could not read the processes' memory") })
    }

    @Test
    fun nfr2_google_play_services_is_reported_apart_with_every_one_of_its_processes() {
        // The owner's PC: gms and gms.unstable mostly in zram, and twelve small processes of
        // other apps, so gms.unstable is not among the ten largest.
        val processes = listOf(
            ProcessPss(700, "com.google.android.gms", 81_000, 74_600, googlePlay = true),
            ProcessPss(701, "com.google.android.gms.unstable", 54_000, 49_000, googlePlay = true),
        ) + (1..12).map { ProcessPss(800 + it, "app$it", 60_000L + it, 1_000) }
        val report = guestMemory(MemInfo.parse(MEMINFO), null, processes)

        assertEquals(2, report.googlePlayProcesses)
        assertEquals(135_000L, report.googlePlayPssKb)
        assertEquals(123_600L, report.googlePlaySwapPssKb)
        assertEquals(123_600L + 12_000L, report.swapPssKb)
        assertEquals(10, report.topProcessesCount)
        assertTrue(report.topProcessesList.single { it.pid == 700 }.googlePlay)
        assertTrue(report.topProcessesList.none { it.pid == 701 })
        assertTrue(report.topProcessesList.filter { it.pid != 700 }.none { it.googlePlay })
    }

    @Test
    fun nfr2_no_process_counts_more_swapped_out_than_its_pss() {
        // The owner's PC on v0.2.2: android.ext.services had 35 MiB of PSS and a swap figure
        // of 61 MiB (every page swapped out, other processes' shares included), and the
        // totals of every process read 0 resident.
        val processes = listOf(
            ProcessPss(5225, "com.android.vending", 169_087, 129_844, googlePlay = true),
            ProcessPss(5294, "com.google.android.gms.unstable", 20_645, 47_804, googlePlay = true),
            ProcessPss(1347, "android.ext.services", 35_395, 61_008),
        )
        val report = guestMemory(MemInfo.parse(MEMINFO), null, processes)

        assertEquals(129_844L + 20_645L + 35_395L, report.swapPssKb)
        assertTrue(report.swapPssKb < report.totalPssKb)
        assertEquals(129_844L + 20_645L, report.googlePlaySwapPssKb)
        assertTrue(report.googlePlaySwapPssKb < report.googlePlayPssKb)
        assertEquals(35_395L, report.topProcessesList.single { it.pid == 1347 }.swapPssKb)
        assertEquals(129_844L, report.topProcessesList.single { it.pid == 5225 }.swapPssKb)
    }

    @Test
    fun nfr2_google_play_services_are_the_google_apps_not_a_google_app_the_user_installed() {
        assertTrue(GooglePlayPackages.isGooglePlay("com.google.android.gms", system = true, fromImage = true))
        assertTrue(GooglePlayPackages.isGooglePlay("com.android.vending", system = true, fromImage = false))
        assertTrue(GooglePlayPackages.isGooglePlay("com.google.android.gsf", system = false, fromImage = false))
        assertTrue(GooglePlayPackages.isGooglePlay("com.google.android.syncadapters.contacts", system = true, fromImage = false))
        assertTrue(GooglePlayPackages.isGooglePlay("com.example.fromimage", system = true, fromImage = true))
        assertFalse(GooglePlayPackages.isGooglePlay("com.google.android.youtube", system = false, fromImage = false))
        assertFalse(GooglePlayPackages.isGooglePlay("com.android.settings", system = true, fromImage = false))
    }
}
