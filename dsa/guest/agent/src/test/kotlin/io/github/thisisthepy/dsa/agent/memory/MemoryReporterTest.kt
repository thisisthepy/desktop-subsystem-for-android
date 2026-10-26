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

    override fun meminfo() = meminfoText

    override fun zramMmStat() = zram

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
}
