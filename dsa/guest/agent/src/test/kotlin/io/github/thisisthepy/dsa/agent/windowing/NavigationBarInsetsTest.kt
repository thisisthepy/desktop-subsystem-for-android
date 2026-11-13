package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.FakeActions
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.requests.InsetGiven
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class NavigationBarInsetsTest {
    private val actions = FakeActions()
    private val log = RecordingLog()
    private val insets = NavigationBarInsets(actions, log)
    private val reach = CaptionInsets.REACH

    @Test
    fun fr22_the_bottom_inset_is_24_dp_at_the_tasks_density() {
        // As getDimensionPixelSize rounds 24 dp: 130 dpi (100%), 196 (150%), 262 (200%), and more.
        assertEquals(24, NavigationBarInsets.heightAt(160))
        assertEquals(20, NavigationBarInsets.heightAt(130))
        assertEquals(29, NavigationBarInsets.heightAt(196))
        assertEquals(39, NavigationBarInsets.heightAt(262))
        assertEquals(49, NavigationBarInsets.heightAt(328))
        assertEquals(72, NavigationBarInsets.heightAt(480))
        for (density in listOf(120, 130, 160, 164, 196, 228, 262, 320, 328, 480, 640)) {
            val px = NavigationBarInsets.heightAt(density)
            assertTrue("$density dpi: $px px", kotlin.math.abs(px - 24f * density / 160) <= 0.5f)
        }
        assertEquals(0, NavigationBarInsets.heightAt(0))
    }

    @Test
    fun fr22_a_task_gets_its_bottom_inset_on_its_first_layout() {
        val at = TaskBounds(100, 40, 900, 1040)

        // Told of the task, before its window has opened or said anything of its title bar.
        insets.update(mapOf(7 to at), mapOf(7 to 262))

        // A band along its bottom edge, 24 dp at 262 dpi, reaching past both sides.
        assertEquals(listOf(7 to TaskBounds(-reach, 1001, reach, 1040)), actions.navigationBarInsets)

        // Nothing new while nothing changes.
        insets.update(mapOf(7 to at), mapOf(7 to 262))
        assertEquals(1, actions.navigationBarInsets.size)
    }

    @Test
    fun fr22_the_bottom_inset_follows_the_tasks_density() {
        val at = TaskBounds(0, 0, 800, 600)
        insets.update(mapOf(7 to at), mapOf(7 to 130))
        assertEquals(7 to TaskBounds(-reach, 580, reach, 600), actions.navigationBarInsets.last())

        // Moved to a 200% monitor: the task draws at 262 dpi, and its band is 24 dp there.
        insets.update(mapOf(7 to at), mapOf(7 to 262))
        assertEquals(7 to TaskBounds(-reach, 561, reach, 600), actions.navigationBarInsets.last())
        assertEquals(2, actions.navigationBarInsets.size)
    }

    @Test
    fun fr22_a_band_that_follows_the_task_needs_nothing_new_when_the_task_is_resized() {
        insets.update(mapOf(7 to TaskBounds(0, 0, 800, 600)), mapOf(7 to 160))
        insets.update(mapOf(7 to TaskBounds(0, 0, 1000, 900)), mapOf(7 to 160))
        insets.update(mapOf(7 to TaskBounds(50, 50, 1000, 700)), mapOf(7 to 160))
        assertEquals(1, actions.navigationBarInsets.size)
    }

    @Test
    fun fr22_a_band_at_a_frame_is_given_again_when_the_tasks_bottom_edge_moves() {
        actions.navigationBarGiven = InsetGiven.AT_FRAME
        insets.update(mapOf(7 to TaskBounds(0, 0, 800, 600)), mapOf(7 to 160))

        // A change of width alone needs nothing new.
        insets.update(mapOf(7 to TaskBounds(0, 0, 1000, 600)), mapOf(7 to 160))
        assertEquals(1, actions.navigationBarInsets.size)

        insets.update(mapOf(7 to TaskBounds(0, 0, 1000, 900)), mapOf(7 to 160))
        assertEquals(7 to TaskBounds(-reach, 876, reach, 900), actions.navigationBarInsets.last())
        assertEquals(2, actions.navigationBarInsets.size)
    }

    @Test
    fun fr22_a_task_that_fills_the_screen_or_is_in_picture_in_picture_has_no_bottom_inset() {
        val at = TaskBounds(0, 0, 800, 600)

        // It filled the screen (or went into picture-in-picture) before it was ever given one.
        insets.update(mapOf(4 to at), emptyMap())
        assertTrue(actions.navigationBarInsets.isEmpty())

        // In a window, then full screen: the band is taken back, once.
        insets.update(mapOf(4 to at), mapOf(4 to 160))
        insets.update(mapOf(4 to at), emptyMap())
        insets.update(mapOf(4 to at), emptyMap())
        assertEquals(listOf(4 to TaskBounds(-reach, 576, reach, 600), 4 to null), actions.navigationBarInsets)

        // Back in a window, it has the band again.
        insets.update(mapOf(4 to at), mapOf(4 to 160))
        assertEquals(4 to TaskBounds(-reach, 576, reach, 600), actions.navigationBarInsets.last())
    }

    @Test
    fun fr22_an_ended_task_is_forgotten_and_a_task_without_a_density_gets_nothing() {
        insets.update(mapOf(3 to TaskBounds(0, 0, 400, 300)), mapOf(3 to 0))
        assertTrue(actions.navigationBarInsets.isEmpty())

        insets.update(mapOf(3 to TaskBounds(0, 0, 400, 300)), mapOf(3 to 160))
        insets.update(emptyMap(), emptyMap())
        actions.navigationBarInsets.clear()
        // A new task 3 is given its own band.
        insets.update(mapOf(3 to TaskBounds(0, 0, 400, 300)), mapOf(3 to 160))
        assertEquals(listOf(3 to TaskBounds(-reach, 276, reach, 300)), actions.navigationBarInsets)
    }

    @Test
    fun fr22_a_bottom_inset_the_framework_refused_is_tried_again_and_logged_once() {
        actions.navigationBarGiven = InsetGiven.REFUSED
        val at = TaskBounds(0, 0, 400, 300)
        insets.update(mapOf(7 to at), mapOf(7 to 160))
        insets.update(mapOf(7 to at), mapOf(7 to 160))
        assertEquals(2, actions.navigationBarInsets.size)
        assertEquals(1, log.lines.count { it.startsWith("warn:") })

        actions.navigationBarGiven = InsetGiven.FOLLOWS_TASK
        insets.update(mapOf(7 to at), mapOf(7 to 160))
        insets.update(mapOf(7 to at), mapOf(7 to 160))
        assertEquals(3, actions.navigationBarInsets.size)
    }
}
