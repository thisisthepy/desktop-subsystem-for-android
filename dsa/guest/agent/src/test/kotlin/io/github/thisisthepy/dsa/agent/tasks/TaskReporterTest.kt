package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.AGENT
import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Envelope
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TaskReporterTest {
    private val sent = mutableListOf<Envelope>()
    private var channelUp = true
    private val reporter = TaskReporter(AGENT) { frame ->
        if (channelUp) sent += decode(frame)
        channelUp
    }

    @Test
    fun fr3_a_new_app_task_is_reported_with_every_field() {
        reporter.update(
            listOf(appTask(7, "org.example.mail", "Mail", TaskBounds(100, 50, 900, 650), focused = true)),
        )

        assertEquals(1, sent.size)
        assertEquals(Envelope.BodyCase.TASK_UPDATED, sent[0].bodyCase)
        val update = sent[0].taskUpdated
        assertEquals(7, update.taskId)
        assertEquals("org.example.mail", update.packageName)
        assertEquals("Mail", update.label)
        assertTrue(update.focused)
        assertEquals(
            listOf(100, 50, 900, 650),
            listOf(update.bounds.left, update.bounds.top, update.bounds.right, update.bounds.bottom),
        )
    }

    @Test
    fun fr3_a_task_stays_its_apps_when_a_permission_dialog_becomes_its_root() {
        // ONE store's task 133: the app asked for a permission and finished its own
        // activities, and the framework named the permission dialog as the task's root.
        val app = "com.skt.skaf.A000Z00040"
        reporter.update(listOf(appTask(133, app, "ONE store", TaskBounds(0, 0, 778, 1293))))
        reporter.update(listOf(appTask(133, "com.android.permissioncontroller", "ONE store", TaskBounds(0, 0, 778, 1293))))
        reporter.update(listOf(appTask(133, app, "ONE store", TaskBounds(0, 0, 778, 1293))))

        assertTrue(sent.none { it.bodyCase == Envelope.BodyCase.TASK_REMOVED })
        val updates = sent.filter { it.bodyCase == Envelope.BodyCase.TASK_UPDATED }
        assertTrue(updates.isNotEmpty())
        assertTrue(updates.all { it.taskUpdated.packageName == app })
        assertEquals(app, reporter.reportedPackage(133))
    }

    @Test
    fun fr22_a_task_carries_its_status_bar_and_is_sent_again_when_it_changes() {
        val dark = appTask(4).copy(statusBar = StatusBarAppearance.DARK, statusBarColor = 0xFF202124.toInt())
        reporter.update(listOf(dark))
        val update = sent.single().taskUpdated
        assertEquals(io.github.thisisthepy.dsa.control.v1.StatusBarAppearance.STATUS_BAR_APPEARANCE_DARK, update.statusBarAppearance)
        assertEquals(0xFF202124.toInt(), update.statusBarColor)

        sent.clear()
        reporter.update(listOf(dark.copy(statusBar = StatusBarAppearance.LIGHT)))
        assertEquals(
            io.github.thisisthepy.dsa.control.v1.StatusBarAppearance.STATUS_BAR_APPEARANCE_LIGHT,
            sent.single().taskUpdated.statusBarAppearance,
        )

        sent.clear()
        reporter.update(listOf(appTask(4)))
        assertEquals(
            io.github.thisisthepy.dsa.control.v1.StatusBarAppearance.STATUS_BAR_APPEARANCE_UNSPECIFIED,
            sent.single().taskUpdated.statusBarAppearance,
        )
    }

    @Test
    fun fr3_an_unchanged_task_is_not_sent_again() {
        reporter.update(listOf(appTask(1), appTask(2)))
        sent.clear()

        reporter.update(listOf(appTask(1), appTask(2)))

        assertEquals(emptyList<Envelope>(), sent)
    }

    @Test
    fun fr3_a_change_in_bounds_package_label_or_focus_sends_the_task_again() {
        val task = appTask(3)
        reporter.update(listOf(task))
        val moved = task.copy(bounds = TaskBounds(0, 0, 640, 480))
        val renamed = moved.copy(label = "Renamed")
        val focused = renamed.copy(focused = true)
        val repackaged = focused.copy(packageName = "org.example.other")
        for (changed in listOf(moved, renamed, focused, repackaged)) {
            sent.clear()
            reporter.update(listOf(changed))
            assertEquals(1, sent.size)
            assertEquals(3, sent[0].taskUpdated.taskId)
        }
        val last = sent[0].taskUpdated
        assertEquals("org.example.other", last.packageName)
        assertEquals("Renamed", last.label)
        assertTrue(last.focused)
        assertEquals(640, last.bounds.right)
    }

    @Test
    fun fr3_focus_moving_between_tasks_updates_both() {
        reporter.update(listOf(appTask(1, focused = true), appTask(2)))
        sent.clear()

        reporter.update(listOf(appTask(2, focused = true), appTask(1)))

        assertEquals(
            setOf(1 to false, 2 to true),
            sent.map { it.taskUpdated.taskId to it.taskUpdated.focused }.toSet(),
        )
    }

    @Test
    fun fr3_an_ended_task_is_reported_removed() {
        reporter.update(listOf(appTask(1), appTask(2)))
        sent.clear()

        reporter.update(listOf(appTask(2)))

        assertEquals(1, sent.size)
        assertEquals(Envelope.BodyCase.TASK_REMOVED, sent[0].bodyCase)
        assertEquals(1, sent[0].taskRemoved.taskId)
        assertEquals(setOf(2), reporter.reported)
    }

    @Test
    fun fr3_only_app_tasks_get_a_window() {
        reporter.update(
            listOf(
                appTask(1, packageName = AGENT),
                appTask(2, packageName = "com.android.settings", kind = TaskKind.OTHER),
                TaskSnapshot(3, TaskKind.APP, null, "", TaskBounds.EMPTY, false),
                appTask(4, packageName = ""),
                appTask(5),
            ),
        )

        assertEquals(listOf(5), sent.map { it.taskUpdated.taskId })
    }

    @Test
    fun fr3_a_task_that_stops_being_an_app_task_loses_its_window() {
        reporter.update(listOf(appTask(1)))
        sent.clear()

        reporter.update(listOf(appTask(1, kind = TaskKind.OTHER)))

        assertEquals(1, sent.single().taskRemoved.taskId)
    }

    @Test
    fun fr3_a_frame_that_could_not_be_sent_is_sent_at_the_next_update() {
        channelUp = false
        reporter.update(listOf(appTask(1)))
        assertTrue(reporter.reported.isEmpty())

        channelUp = true
        reporter.update(listOf(appTask(1)))
        assertEquals(listOf(1), sent.map { it.taskUpdated.taskId })

        channelUp = false
        reporter.update(emptyList())
        channelUp = true
        reporter.update(emptyList())
        assertEquals(1, sent.last().taskRemoved.taskId)
        assertTrue(reporter.reported.isEmpty())
    }

    @Test
    fun fr3_a_new_connection_resends_every_task_and_ends_the_tasks_gone_meanwhile() {
        reporter.update(listOf(appTask(1), appTask(2)))
        channelUp = false
        reporter.update(listOf(appTask(2)))
        sent.clear()
        channelUp = true

        reporter.resend()
        reporter.update(listOf(appTask(2)))

        assertEquals(1, sent.single { it.bodyCase == Envelope.BodyCase.TASK_REMOVED }.taskRemoved.taskId)
        assertEquals(2, sent.single { it.bodyCase == Envelope.BodyCase.TASK_UPDATED }.taskUpdated.taskId)

        sent.clear()
        reporter.update(listOf(appTask(2)))
        assertEquals(emptyList<Envelope>(), sent)
    }
}
