package io.github.thisisthepy.dsa.agent.splash

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class StartingWindowsTest {
    private class FakeWindow(val name: String) : StartingWindow {
        var removed = 0
        val redrawnAt = mutableListOf<Int>()
        override fun remove() {
            removed++
        }

        override fun redraw(densityDpi: Int) {
            redrawnAt += densityDpi
        }
    }

    private val created = mutableListOf<FakeWindow>()
    private val windows = StartingWindows()

    private fun window(name: String): () -> StartingWindow? = {
        FakeWindow(name).also { created += it }
    }

    @Test
    fun fr3_a_starting_window_is_removed_when_the_framework_says() {
        windows.add(7, 160, window("a"))
        assertEquals(setOf(7), windows.tasks())
        windows.remove(7)
        assertEquals(1, created.single().removed)
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_second_starting_window_for_a_task_replaces_the_first() {
        windows.add(7, 160, window("a"))
        windows.add(7, 160, window("b"))
        assertEquals(1, created[0].removed)
        assertEquals(0, created[1].removed)
        windows.remove(7)
        assertEquals(1, created[1].removed)
    }

    @Test
    fun fr3_a_removal_for_a_task_without_a_window_does_nothing() {
        windows.remove(3)
        windows.add(7, 160, window("a"))
        windows.remove(3)
        assertEquals(0, created.single().removed)
    }

    @Test
    fun fr3_a_window_that_could_not_be_added_is_not_kept() {
        windows.add(7, 160) { null }
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_failing_window_does_not_stop_the_others_from_being_removed() {
        val failing = object : StartingWindow {
            override fun remove() = throw IllegalStateException("view already gone")
        }
        windows.add(1, 160) { failing }
        windows.add(2, 160, window("b"))
        val errors = windows.clear()
        assertEquals(1, errors.size)
        assertEquals(1, created.single().removed)
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_task_that_ends_takes_its_starting_window_with_it() {
        windows.add(7, 160, window("a"))
        assertTrue(windows.remove(7).isEmpty())
        assertFalse(7 in windows.tasks())
    }

    @Test
    fun fr4_a_starting_window_is_drawn_again_when_its_task_takes_its_monitors_density() {
        // Drawn at the display's density, before the host window told the task its own.
        windows.add(7, 160, window("a"))
        assertTrue(windows.densityChanged(7, 320).isEmpty())
        assertEquals(listOf(320), created.single().redrawnAt)
        assertEquals(320, windows.densityOf(7))
        // A move to a 125% monitor draws it once more, at that density.
        windows.densityChanged(7, 200)
        assertEquals(listOf(320, 200), created.single().redrawnAt)
        // The window stays: it is drawn again in place, not replaced.
        assertEquals(0, created.single().removed)
    }

    @Test
    fun fr4_a_starting_window_at_its_tasks_density_is_not_drawn_again() {
        windows.add(7, 320, window("a"))
        windows.densityChanged(7, 320)
        windows.densityChanged(7, 0)
        windows.densityChanged(3, 240)
        assertEquals(emptyList<Int>(), created.single().redrawnAt)
    }

    @Test
    fun fr4_a_window_that_fails_to_be_drawn_again_is_tried_on_the_next_change() {
        var fail = true
        val drawn = mutableListOf<Int>()
        val flaky = object : StartingWindow {
            override fun remove() {}
            override fun redraw(densityDpi: Int) {
                if (fail) throw IllegalStateException("view detached")
                drawn += densityDpi
            }
        }
        windows.add(7, 160) { flaky }
        assertEquals(1, windows.densityChanged(7, 320).size)
        assertEquals(160, windows.densityOf(7))
        fail = false
        assertTrue(windows.densityChanged(7, 320).isEmpty())
        assertEquals(listOf(320), drawn)
    }
}
