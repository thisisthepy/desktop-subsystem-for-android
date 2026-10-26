package io.github.thisisthepy.dsa.agent.requests

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.tasks.Messages
import io.github.thisisthepy.dsa.agent.windowing.Size
import io.github.thisisthepy.dsa.control.v1.CloseTask
import io.github.thisisthepy.dsa.control.v1.Envelope
import io.github.thisisthepy.dsa.control.v1.LaunchApp
import io.github.thisisthepy.dsa.control.v1.ResizeTask
import io.github.thisisthepy.dsa.control.v1.SetTaskDensity
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

    private fun resize(taskId: Int, width: Int, height: Int, live: Boolean = false) = Envelope.newBuilder()
        .setResizeTask(ResizeTask.newBuilder().setTaskId(taskId).setWidth(width).setHeight(height).setLive(live))
        .build().toByteArray()

    private fun density(taskId: Int, densityDpi: Int) = Envelope.newBuilder()
        .setSetTaskDensity(SetTaskDensity.newBuilder().setTaskId(taskId).setDensityDpi(densityDpi))
        .build().toByteArray()

    @Test
    fun fr4_a_resize_and_a_density_for_a_reported_task_are_handed_back() {
        reported += 3

        assertEquals(RequestResult.Resize(3, Size(1280, 720)), requests.handle(resize(3, 1280, 720)))
        assertEquals(RequestResult.Density(3, 320), requests.handle(density(3, 320)))
    }

    @Test
    fun fr4_a_resize_says_whether_the_user_is_still_dragging() {
        reported += 3

        assertEquals(RequestResult.Resize(3, Size(1280, 720), live = true), requests.handle(resize(3, 1280, 720, live = true)))
        // The end of the drag is handed back even when the size did not change.
        assertEquals(RequestResult.Resize(3, Size(1280, 720), live = false), requests.handle(resize(3, 1280, 720)))
        assertTrue(actions.resized.isEmpty())
    }

    @Test
    fun fr4_sizes_for_unknown_tasks_and_impossible_sizes_are_refused() {
        reported += 3

        assertTrue(requests.handle(resize(4, 1280, 720)) is RequestResult.SizeRefused)
        assertTrue(requests.handle(density(4, 320)) is RequestResult.SizeRefused)
        assertTrue(requests.handle(resize(3, 0, 720)) is RequestResult.Malformed)
        // An unsigned size past Int.MAX_VALUE.
        assertTrue(requests.handle(resize(3, -1, 720)) is RequestResult.Malformed)
        assertTrue(requests.handle(density(3, 0)) is RequestResult.Malformed)
        assertTrue(requests.handle(density(3, 5000)) is RequestResult.Malformed)
    }

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
