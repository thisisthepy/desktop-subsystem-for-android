package io.github.thisisthepy.dsa.agent

import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM
import io.github.thisisthepy.dsa.control.v1.CloseTask
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.LaunchApp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class AgentTest {
    private val scheduler = FakeScheduler()
    private val tasks = FakeTasks()
    private val actions = FakeActions()
    private val locator = FakeLocator()
    private val daemon = FakeEndpoint()
    private val windowing = FakeWindowing()
    private val log = RecordingLog()

    // Where Android's freeform launch put both Settings and Clock on a real machine.
    private val defaultPlace = TaskBounds(686, 53, 1234, 1027)

    private fun started(source: TaskSource = tasks): Agent {
        locator.up = daemon
        val agent = Agent(AGENT, source, actions, locator, windowing, scheduler, log)
        agent.start()
        scheduler.runDue()
        return agent
    }

    @Test
    fun fr3_at_boot_the_display_is_freeform_and_running_app_tasks_are_reported() {
        tasks.running = listOf(appTask(1), appTask(2, packageName = AGENT))

        started()

        assertEquals(WINDOWING_MODE_FREEFORM, windowing.mode)
        assertEquals(listOf(1), daemon.sentMessages().map { it.taskUpdated.taskId })
    }

    @Test
    fun fr3_a_launched_app_is_reported_once_its_task_appears() {
        started()
        actions.launchable = setOf("org.example.mail")

        daemon.deliver(
            Envelope.newBuilder().setLaunchApp(LaunchApp.newBuilder().setPackageName("org.example.mail")).build().toByteArray(),
        )
        scheduler.runDue()
        assertEquals(listOf("org.example.mail"), actions.launched)

        tasks.change(listOf(appTask(9, packageName = "org.example.mail", label = "Mail", focused = true)))
        scheduler.runDue()

        val update = daemon.sentMessages().single().taskUpdated
        assertEquals(9, update.taskId)
        assertEquals("org.example.mail", update.packageName)
        assertEquals("Mail", update.label)
        assertTrue(update.focused)
    }

    @Test
    fun fr3_closing_a_window_ends_its_task_and_reports_it_removed() {
        tasks.running = listOf(appTask(4))
        started()
        actions.closable = setOf(4)

        daemon.deliver(Envelope.newBuilder().setCloseTask(CloseTask.newBuilder().setTaskId(4)).build().toByteArray())
        scheduler.runDue()
        assertEquals(listOf(4), actions.closed)

        tasks.change(emptyList())
        scheduler.runDue()
        assertEquals(4, daemon.sentMessages().last().taskRemoved.taskId)
    }

    @Test
    fun fr3_a_burst_of_task_changes_is_read_once() {
        var reads = 0
        val counting = object : TaskSource by tasks {
            override fun snapshot() = tasks.snapshot().also { reads++ }
        }
        started(counting)
        reads = 0

        repeat(5) { tasks.change(listOf(appTask(1))) }
        scheduler.runDue()

        assertEquals(1, reads)
    }

    @Test
    fun fr3_after_the_daemon_restarts_the_host_is_told_every_task_again() {
        tasks.running = listOf(appTask(1))
        started()
        locator.up = null
        daemon.die()
        scheduler.runDue()

        val restarted = FakeEndpoint()
        locator.up = restarted
        scheduler.advance(1000)

        assertEquals(listOf(1), restarted.sentMessages().map { it.taskUpdated.taskId })
    }

    @Test
    fun fr3_two_apps_launched_at_the_same_place_get_places_of_their_own() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace))
        started()

        tasks.change(listOf(appTask(5, bounds = defaultPlace), appTask(4, bounds = defaultPlace)))
        scheduler.runDue()

        val target = TaskBounds(0, 0, 548, 974)
        assertEquals(listOf(5 to target), actions.moved)
        // The host never hears of the two tasks overlapping: the moving task is reported at
        // the place it is going to.
        val reported = daemon.sentMessages().associate { it.taskUpdated.taskId to it.taskUpdated.bounds }
        assertEquals(686, reported.getValue(4).left)
        assertEquals(0, reported.getValue(5).left)
        assertEquals(548, reported.getValue(5).right)
    }

    @Test
    fun fr3_a_moved_task_is_not_moved_or_reported_again_once_it_lands() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()
        val sent = daemon.sent.size

        // The window manager applies the move after the call returns, without telling the
        // task stack listener; the agent's next check sees it.
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = TaskBounds(0, 0, 548, 974)))
        scheduler.advance(2000)

        assertEquals(1, actions.moved.size)
        assertEquals(sent, daemon.sent.size)
    }

    @Test
    fun fr3_a_move_that_never_lands_is_given_up_and_the_task_reported_where_it_is() {
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()

        scheduler.advance(5000)

        assertEquals(1, actions.moved.size)
        val last = daemon.sentMessages().last().taskUpdated
        assertEquals(5, last.taskId)
        assertEquals(686, last.bounds.left)
        assertTrue(log.lines.any { it.startsWith("warn: task 5 did not move") })
    }

    @Test
    fun fr3_a_task_appearing_on_a_full_display_goes_where_it_overlaps_least() {
        val full = listOf(
            appTask(1, bounds = TaskBounds(0, 0, 548, 974)),
            appTask(2, bounds = TaskBounds(548, 0, 1096, 974)),
            appTask(3, bounds = TaskBounds(1096, 0, 1644, 974)),
        )
        tasks.running = full
        started()

        tasks.change(full + appTask(4, bounds = defaultPlace))
        scheduler.runDue()

        assertEquals(listOf(4 to TaskBounds(1372, 106, 1920, 1080)), actions.moved)
        assertTrue(log.lines.any { it.startsWith("warn: the 1920x1080 display has no free place for task 4") })
    }

    @Test
    fun nfr5_a_failed_move_does_not_stop_the_agent() {
        actions.onMove = { _, _ -> throw IllegalArgumentException("cannot resize") }
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()
        scheduler.advance(1000)

        assertEquals(1, actions.moved.size)
        assertEquals(listOf(4, 5), daemon.sentMessages().map { it.taskUpdated.taskId }.sorted())
    }

    @Test
    fun nfr5_an_unreadable_display_size_leaves_tasks_in_place_and_reported() {
        windowing.size = null
        tasks.running = listOf(appTask(4, bounds = defaultPlace), appTask(5, bounds = defaultPlace))
        started()

        assertTrue(actions.moved.isEmpty())
        assertEquals(listOf(4, 5), daemon.sentMessages().map { it.taskUpdated.taskId }.sorted())
    }

    @Test
    fun nfr5_a_bad_frame_does_not_stop_the_agent() {
        tasks.running = listOf(appTask(1))
        started()

        daemon.deliver(byteArrayOf(0x0a, 0x7f))
        scheduler.runDue()
        tasks.change(listOf(appTask(1), appTask(2)))
        scheduler.runDue()

        assertEquals(listOf(1, 2), daemon.sentMessages().map { it.taskUpdated.taskId })
    }
}
