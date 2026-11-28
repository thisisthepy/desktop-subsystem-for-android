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
    var total = 4096L * 1024

    /** The running processes of each package, with what each holds (PSS, KiB). */
    val running = mutableMapOf<String, MutableMap<Int, Long>>()

    /** Packages whose processes outlive one stop. */
    val stubborn = mutableSetOf<String>()

    override fun hasForegroundService(packageName: String) = packageName in foreground

    /** Packages one of whose activities the user sees. */
    val visible = mutableSetOf<String>()

    override fun showsActivity(packageName: String) = packageName in visible

    override fun stop(packageName: String) {
        stopped += packageName
        if (stubborn.remove(packageName)) return
        free += running.remove(packageName)?.values?.sum() ?: 0L
    }

    /** Ends the package's processes the way removing its task does, before the agent stops it. */
    fun taskRemoved(packageName: String) {
        free += running.remove(packageName)?.values?.sum() ?: 0L
    }

    override fun memory() = GuestMemInfo(totalKb = total, freeKb = free, availableKb = free + 500L * 1024)

    override fun packageMemory(packageName: String): PackageMemory {
        val pids = running[packageName].orEmpty()
        return PackageMemory(pids.keys.sorted(), pids.values.sum(), 0)
    }

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
        processes.running["org.example.notes"] = mutableMapOf(41 to 40L * 1024, 42 to 20L * 1024)
        apps.tasks(setOf("org.example.notes", "org.example.clock"))
        apps.closing("org.example.notes")
        apps.tasks(setOf("org.example.clock"))
        assertEquals(listOf("org.example.notes"), processes.stopped)
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        assertTrue(
            log.lines.toString(),
            log.lines.contains(
                "info: closed org.example.notes: its 2 processes held 60 MiB (PSS, 0 MiB of it in zram) and ended; " +
                    "the guest's free memory rose 60 MiB (available memory +60 MiB) in ${ClosedApps.SETTLE_MILLIS} ms, " +
                    "counted from the window's close",
            ),
        )
    }

    @Test
    fun nfr2_what_a_closed_app_held_is_counted_from_its_windows_close_before_its_task_ends_it() {
        // Removing the task ends the app's processes before the agent hears that it has no
        // window: what they held was counted as nothing given back.
        processes.running["org.example.game"] = mutableMapOf(77 to 900L * 1024)
        apps.tasks(setOf("org.example.game"))
        apps.closing("org.example.game")
        processes.taskRemoved("org.example.game")
        scheduler.advance(300)
        apps.tasks(emptySet())
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        val line = log.lines.single { it.startsWith("info: closed org.example.game:") }
        assertTrue(line, line.contains("its 1 process held 900 MiB (PSS, 0 MiB of it in zram) and ended"))
        assertTrue(line, line.contains("the guest's free memory rose 900 MiB"))
        assertTrue(line, line.contains("in ${ClosedApps.SETTLE_MILLIS + 300} ms, counted from the window's close"))
    }

    @Test
    fun nfr2_memory_the_host_took_back_from_the_vm_is_named_and_a_process_that_outlived_the_end_is_ended_again() {
        processes.running["org.example.video"] = mutableMapOf(90 to 300L * 1024)
        processes.stubborn += "org.example.video"
        apps.tasks(setOf("org.example.video"))
        apps.closing("org.example.video")
        processes.total -= 200L * 1024
        apps.tasks(emptySet())
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        val line = log.lines.single { it.startsWith("info: closed org.example.video:") }
        assertTrue(line, line.contains("and the host took 200 MiB back from the VM"))
        assertTrue(line, line.endsWith("; still running: pid 90"))
        assertEquals(listOf("org.example.video", "org.example.video"), processes.stopped)
        assertTrue(processes.running.isEmpty())
    }

    @Test
    fun nfr2_an_app_whose_task_root_became_a_permission_dialog_it_opened_is_not_stopped() {
        // ONE store's task 133: the app asked for a permission and finished its own
        // activities, so the framework named the permission dialog as the task's root and the
        // task was reported as com.android.permissioncontroller's. The app's activities were
        // still in the task, under the dialog the user was answering.
        val app = "com.skt.skaf.A000Z00040"
        val dialog = "com.android.permissioncontroller"
        processes.running[app] = mutableMapOf(7218 to 169L * 1024)
        apps.tasks(setOf(app), inTasks = setOf(app))
        apps.tasks(setOf(dialog), inTasks = setOf(dialog, app))
        scheduler.advance(ClosedApps.SETTLE_MILLIS * (ClosedApps.MAX_RECHECKS + 1))
        assertTrue(processes.stopped.toString(), processes.stopped.isEmpty())
        // The dialog answered and the task ended by itself: no window was closed by the user,
        // so neither is stopped (build 1.0.1-dev.12 stopped both).
        apps.tasks(emptySet(), inTasks = emptySet())
        assertTrue(processes.stopped.toString(), processes.stopped.isEmpty())
        // The user closes the window of a task of the app: it is ended then.
        apps.tasks(setOf(app), inTasks = setOf(app))
        apps.closing(app)
        apps.tasks(emptySet(), inTasks = emptySet())
        assertEquals(listOf(app), processes.stopped)
        assertTrue(log.lines.toString(), log.lines.contains("info: force-stopping $app: it has no task window and no activity in any task"))
    }

    @Test
    fun nfr2_an_app_opened_in_another_apps_task_is_not_stopped_again_after_its_own_task_closed() {
        // Settings' own task closed and Settings was ended; its process outlived the end, and
        // meanwhile a test app opened Settings' screens in the test app's own task.
        val settings = "com.android.settings"
        val probe = "io.github.thisisthepy.dsa.fr34probe"
        processes.running[settings] = mutableMapOf(5629 to 76L * 1024)
        processes.stubborn += settings
        apps.tasks(setOf(settings, probe), inTasks = setOf(settings, probe))
        apps.closing(settings)
        apps.tasks(setOf(probe), inTasks = setOf(probe))
        assertEquals(listOf(settings), processes.stopped)
        processes.running[settings] = mutableMapOf(5673 to 76L * 1024)
        apps.tasks(setOf(probe), inTasks = setOf(probe, settings))
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        assertEquals(listOf(settings), processes.stopped)
    }

    @Test
    fun nfr2_an_app_that_still_shows_an_activity_is_stopped_only_once_it_stops_showing() {
        processes.running["org.example.notes"] = mutableMapOf(41 to 40L * 1024)
        processes.visible += "org.example.notes"
        apps.tasks(setOf("org.example.notes"))
        apps.closing("org.example.notes")
        apps.tasks(emptySet())
        assertTrue(processes.stopped.isEmpty())
        processes.visible.clear()
        scheduler.advance(ClosedApps.SETTLE_MILLIS)
        assertEquals(listOf("org.example.notes"), processes.stopped)
    }

    @Test
    fun nfr2_an_app_with_a_placed_widget_or_a_foreground_service_keeps_running() {
        widgets += "org.example.clock"
        processes.foreground += "org.example.music"
        apps.tasks(setOf("org.example.clock", "org.example.music"))
        apps.closing("org.example.clock")
        apps.closing("org.example.music")
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
    fun nfr2_an_app_whose_task_ends_without_the_users_close_is_never_stopped() {
        // Build 1.0.1-dev.12 force-stopped the permission controller, the credential manager
        // and ONE store's helper as their tasks ended on their own.
        for (packageName in listOf("com.android.permissioncontroller", "com.android.credentialmanager", "com.skt.skaf.OA00018282")) {
            apps.tasks(setOf(packageName), inTasks = setOf(packageName))
            apps.tasks(emptySet())
        }
        scheduler.advance(ClosedApps.SETTLE_MILLIS * (ClosedApps.MAX_RECHECKS + 1))
        assertTrue(processes.stopped.toString(), processes.stopped.isEmpty())
    }

    @Test
    fun nfr2_an_app_reopened_while_its_close_waits_on_a_shown_activity_is_not_stopped() {
        // NAVER: closed, then opened again within moments.
        val naver = "com.nhn.android.search"
        processes.visible += naver
        apps.tasks(setOf(naver))
        apps.closing(naver)
        apps.tasks(emptySet())
        apps.tasks(setOf(naver))
        processes.visible.clear()
        scheduler.advance(ClosedApps.SETTLE_MILLIS * (ClosedApps.MAX_RECHECKS + 1))
        assertTrue(processes.stopped.toString(), processes.stopped.isEmpty())
    }

    @Test
    fun nfr2_an_app_that_still_has_a_window_is_not_ended() {
        apps.tasks(setOf("org.example.notes"))
        apps.tasks(setOf("org.example.notes"))
        assertTrue(processes.stopped.isEmpty())
    }
}
