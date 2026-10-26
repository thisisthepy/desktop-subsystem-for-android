package io.github.thisisthepy.dsa.agent.splash

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Test

class StartingWindowsTest {
    private class FakeWindow(val name: String) : StartingWindow {
        var removed = 0
        override fun remove() {
            removed++
        }
    }

    private val created = mutableListOf<FakeWindow>()
    private val windows = StartingWindows()

    private fun window(name: String): () -> StartingWindow? = {
        FakeWindow(name).also { created += it }
    }

    @Test
    fun fr3_a_starting_window_is_removed_when_the_framework_says() {
        windows.add(7, window("a"))
        assertEquals(setOf(7), windows.tasks())
        windows.remove(7)
        assertEquals(1, created.single().removed)
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_second_starting_window_for_a_task_replaces_the_first() {
        windows.add(7, window("a"))
        windows.add(7, window("b"))
        assertEquals(1, created[0].removed)
        assertEquals(0, created[1].removed)
        windows.remove(7)
        assertEquals(1, created[1].removed)
    }

    @Test
    fun fr3_a_removal_for_a_task_without_a_window_does_nothing() {
        windows.remove(3)
        windows.add(7, window("a"))
        windows.remove(3)
        assertEquals(0, created.single().removed)
    }

    @Test
    fun fr3_a_window_that_could_not_be_added_is_not_kept() {
        windows.add(7) { null }
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_failing_window_does_not_stop_the_others_from_being_removed() {
        val failing = object : StartingWindow {
            override fun remove() = throw IllegalStateException("view already gone")
        }
        windows.add(1) { failing }
        windows.add(2, window("b"))
        val errors = windows.clear()
        assertEquals(1, errors.size)
        assertEquals(1, created.single().removed)
        assertEquals(emptySet<Int>(), windows.tasks())
    }

    @Test
    fun fr3_a_task_that_ends_takes_its_starting_window_with_it() {
        windows.add(7, window("a"))
        assertTrue(windows.remove(7).isEmpty())
        assertFalse(7 in windows.tasks())
    }
}
