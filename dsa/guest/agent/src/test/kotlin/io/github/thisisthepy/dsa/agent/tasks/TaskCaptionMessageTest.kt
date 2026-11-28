package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.appTask
import io.github.thisisthepy.dsa.agent.decode
import io.github.thisisthepy.dsa.control.v1.Rect
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class TaskCaptionMessageTest {
    @Test
    fun fr22_an_app_that_draws_its_own_title_bar_says_so_with_its_controls() {
        val task = appTask(1).copy(
            transparentCaption = true,
            captionInteractive = listOf(TaskBounds(40, 0, 400, 31)),
        )
        val update = decode(Messages.taskUpdated(task)).taskUpdated

        assertTrue(update.transparentCaption)
        assertEquals(
            listOf(Rect.newBuilder().setLeft(40).setTop(0).setRight(400).setBottom(31).build()),
            update.captionInteractiveList,
        )
        // Withdrawn, the controls no longer go with it.
        val withdrawn = decode(Messages.taskUpdated(task.copy(transparentCaption = false))).taskUpdated
        assertFalse(withdrawn.transparentCaption)
        assertTrue(withdrawn.captionInteractiveList.isEmpty())
    }

    @Test
    fun fr22_a_task_says_that_it_starts_below_the_title_bar() {
        assertTrue(decode(Messages.taskUpdated(appTask(1).copy(belowTitleBar = true))).taskUpdated.belowTitleBar)
        assertFalse(decode(Messages.taskUpdated(appTask(1))).taskUpdated.belowTitleBar)
    }

    @Test
    fun fr4_a_task_says_whether_its_top_activity_handles_a_change_of_density() {
        // configChanges="density|orientation|screenSize" and "orientation|screenSize|keyboardHidden".
        assertTrue(handlesDensityChange(0x1000 or 0x80 or 0x400))
        assertFalse(handlesDensityChange(0x80 or 0x400 or 0x20))
        assertTrue(
            decode(Messages.taskUpdated(appTask(1).copy(handlesDensityChange = true))).taskUpdated.handlesDensityChange,
        )
        assertFalse(decode(Messages.taskUpdated(appTask(1))).taskUpdated.handlesDensityChange)
    }

    @Test
    fun fr4_a_task_says_whether_it_can_take_a_new_density_without_a_relaunch() {
        assertTrue(
            decode(Messages.taskUpdated(appTask(1).copy(forcedDensityAvailable = true))).taskUpdated.forcedDensityAvailable,
        )
        assertFalse(decode(Messages.taskUpdated(appTask(1))).taskUpdated.forcedDensityAvailable)
    }

    @Test
    fun fr22_the_icon_becomes_a_back_button_when_the_task_can_go_back() {
        assertTrue(decode(Messages.taskUpdated(appTask(1).copy(canGoBack = true))).taskUpdated.canGoBack)
        assertFalse(decode(Messages.taskUpdated(appTask(1))).taskUpdated.canGoBack)
    }
}
