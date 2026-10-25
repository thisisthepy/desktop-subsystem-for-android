package io.github.thisisthepy.dsa.agent

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

    private fun started(source: TaskSource = tasks): Agent {
        locator.up = daemon
        val agent = Agent(AGENT, source, actions, locator, windowing, scheduler, RecordingLog())
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
