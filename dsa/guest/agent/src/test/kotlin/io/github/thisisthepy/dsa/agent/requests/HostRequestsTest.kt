package io.github.thisisthepy.dsa.agent.requests

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.tasks.Messages
import io.github.thisisthepy.dsa.control.v1.CloseTask
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.LaunchApp
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class HostRequestsTest {
    private val actions = FakeActions()
    private val reported = mutableSetOf<Int>()
    private val requests = HostRequests(AGENT, actions) { it in reported }

    private fun launch(packageName: String) =
        Envelope.newBuilder().setLaunchApp(LaunchApp.newBuilder().setPackageName(packageName)).build().toByteArray()

    private fun close(taskId: Int) =
        Envelope.newBuilder().setCloseTask(CloseTask.newBuilder().setTaskId(taskId)).build().toByteArray()

    @Test
    fun fr3_launch_app_starts_the_package() {
        actions.launchable = setOf("org.example.mail")

        val result = requests.handle(launch("org.example.mail"))

        assertEquals(RequestResult.Launched("org.example.mail"), result)
        assertEquals(listOf("org.example.mail"), actions.launched)
    }

    @Test
    fun fr3_launch_app_of_a_package_with_no_launcher_activity_fails() {
        val result = requests.handle(launch("org.example.none"))

        assertTrue(result is RequestResult.LaunchFailed)
    }

    @Test
    fun fr3_launch_app_naming_no_package_or_the_agent_starts_nothing() {
        assertTrue(requests.handle(launch("")) is RequestResult.Malformed)
        assertTrue(requests.handle(launch(AGENT)) is RequestResult.LaunchFailed)
        assertEquals(emptyList<String>(), actions.launched)
    }

    @Test
    fun fr3_close_task_ends_a_reported_task() {
        reported += 12
        actions.closable = setOf(12)

        val result = requests.handle(close(12))

        assertEquals(RequestResult.Closed(12), result)
        assertEquals(listOf(12), actions.closed)
    }

    @Test
    fun fr3_close_task_of_a_task_the_host_was_never_told_about_is_refused() {
        actions.closable = setOf(3)

        val result = requests.handle(close(3))

        assertTrue(result is RequestResult.CloseRefused)
        assertEquals(emptyList<Int>(), actions.closed)
    }

    @Test
    fun fr3_close_task_the_framework_cannot_end_is_refused() {
        reported += 4

        assertTrue(requests.handle(close(4)) is RequestResult.CloseRefused)
        assertEquals(listOf(4), actions.closed)
    }

    @Test
    fun fr3_task_messages_of_other_guest_clients_are_not_requests() {
        val result = requests.handle(Messages.taskUpdated(appTask(5)))

        assertEquals(RequestResult.NotForTheAgent(Envelope.BodyCase.TASK_UPDATED), result)
        assertTrue(actions.launched.isEmpty() && actions.closed.isEmpty())
    }

    @Test
    fun nfr5_a_frame_that_does_not_decode_is_dropped() {
        val garbage = byteArrayOf(0x52, 0x7f, 0x01)

        assertTrue(requests.handle(garbage) is RequestResult.Malformed)
        assertTrue(requests.handle(ByteArray(0)) is RequestResult.Malformed)
        assertTrue(actions.launched.isEmpty() && actions.closed.isEmpty())
    }
}
