package io.github.thisisthepy.dsa.agent.memory

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.agent.RecordingLog
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class FakeProcesses : AppProcesses {
    val stopped = mutableListOf<String>()
    val foreground = mutableSetOf<String>()
    var free = 100L * 1024

    override fun hasForegroundService(packageName: String) = packageName in foreground

    override fun stop(packageName: String) {
        stopped += packageName
        free += 60L * 1024
    }

    override fun freeKb() = free

    val disabled = mutableListOf<String>()
    var cachedLimit: Int? = null

    override fun disable(packageName: String): Boolean {
        disabled += packageName
        return true
    }

    override fun limitCachedProcesses(max: Int) {
        cachedLimit = max
    }
}

class ClosedAppsTest {
    private val processes = FakeProcesses()
    private val scheduler = FakeScheduler()
    private val log = RecordingLog()
    private val widgets = mutableSetOf<String>()
    private val apps = ClosedApps(processes, { it in widgets }, scheduler, log, clock = { scheduler.now })

    @Test
    fun nfr2_an_app_whose_last_window_closed_is_ended_and_its_return_logged() {
        apps.tasks(setOf("org.example.notes", "org.example.clock"))
        apps.tasks(setOf("org.example.clock"))
        assertEquals(listOf("org.example.notes"), processes.stopped)
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        assertTrue(log.lines.toString(), log.lines.contains("info: closed org.example.notes: returned 60 MiB in ${ClosedApps.SETTLE_MILLIS} ms"))
    }

    @Test
    fun nfr2_an_app_with_a_placed_widget_or_a_foreground_service_keeps_running() {
        widgets += "org.example.clock"
        processes.foreground += "org.example.music"
        apps.tasks(setOf("org.example.clock", "org.example.music"))
        apps.tasks(emptySet())
        assertTrue(processes.stopped.isEmpty())
    }

    @Test
    fun nfr2_the_hosts_stop_ends_even_an_app_with_a_foreground_service() {
        processes.foreground += "org.example.music"
        assertFalse(apps.stop("org.example.music", force = false))
        assertTrue(apps.stop("org.example.music", force = true))
        assertEquals(listOf("org.example.music"), processes.stopped)
    }

    @Test
    fun nfr2_the_guest_keeps_few_cached_processes_and_turns_off_phone_only_apps() {
        apps.configure()
        assertEquals(ClosedApps.MAX_CACHED_PROCESSES, processes.cachedLimit)
        assertEquals(listOf("com.android.google.gce.gceservice", "com.android.devicelockcontroller"), processes.disabled)
    }

    @Test
    fun nfr2_an_app_that_still_has_a_window_is_not_ended() {
        apps.tasks(setOf("org.example.notes"))
        apps.tasks(setOf("org.example.notes"))
        assertTrue(processes.stopped.isEmpty())
    }
}
