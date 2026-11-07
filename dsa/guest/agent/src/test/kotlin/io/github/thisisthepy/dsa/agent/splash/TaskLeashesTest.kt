package io.github.thisisthepy.dsa.agent.splash

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Registering the organizer that draws starting windows makes the window manager stop
 * placing every task's surface: an organized task's surface position and crop are left to
 * the organizer. So the agent places and crops each task's surface where the window manager
 * would. The crop bounds where the task's windows take pointer input.
 */
class TaskLeashesTest {
    private sealed interface Call {
        data class Place(val leash: String, val x: Int, val y: Int, val width: Int, val height: Int) : Call
        data class Release(val leash: String) : Call
    }

    private val calls = mutableListOf<Call>()
    private val leashes = TaskLeashes(object : LeashSurfaces<String> {
        override fun place(leash: String, x: Int, y: Int, width: Int, height: Int) {
            calls += Call.Place(leash, x, y, width, height)
        }

        override fun release(leash: String) {
            calls += Call.Release(leash)
        }
    })

    @Test
    fun fr3_an_organized_task_surface_stays_where_its_bounds_are() {
        leashes.appeared(5, "five", LeashPlace(686, 53, 800, 600))
        assertEquals(listOf(Call.Place("five", 686, 53, 800, 600)), calls)
    }

    @Test
    fun fr5_each_task_surface_is_cropped_to_its_own_bounds_so_a_click_reaches_the_task_under_it() {
        // Two tasks side by side. Without a crop the upper one's touch-modal window would
        // take the click meant for the other at a point outside its own window.
        leashes.appeared(6, "clock", LeashPlace(0, 0, 900, 700))
        leashes.appeared(10, "calendar", LeashPlace(900, 0, 800, 600))
        assertEquals(
            listOf(Call.Place("clock", 0, 0, 900, 700), Call.Place("calendar", 900, 0, 800, 600)),
            calls,
        )
    }

    @Test
    fun fr5_a_resized_task_takes_input_across_its_new_bounds() {
        leashes.appeared(5, "five", LeashPlace(10, 20, 800, 600))
        calls.clear()
        leashes.changed(5, LeashPlace(10, 20, 1000, 750))
        assertEquals(listOf(Call.Place("five", 10, 20, 1000, 750)), calls)
    }

    @Test
    fun fr3_a_moved_task_surface_follows_its_bounds() {
        leashes.appeared(5, "five", LeashPlace(686, 53, 800, 600))
        calls.clear()
        leashes.changed(5, LeashPlace(0, 0, 800, 600))
        assertEquals(listOf(Call.Place("five", 0, 0, 800, 600)), calls)
    }

    @Test
    fun fr3_a_change_that_neither_moves_nor_resizes_the_task_leaves_its_surface_alone() {
        leashes.appeared(5, "five", LeashPlace(686, 53, 800, 600))
        calls.clear()
        leashes.changed(5, LeashPlace(686, 53, 800, 600))
        assertEquals(emptyList<Call>(), calls)
    }

    @Test
    fun fr3_a_change_for_an_unknown_task_is_ignored() {
        leashes.changed(9, LeashPlace(1, 2, 3, 4))
        assertEquals(emptyList<Call>(), calls)
    }

    @Test
    fun fr3_a_task_that_ends_releases_its_surface() {
        leashes.appeared(5, "five", LeashPlace(0, 0, 10, 10))
        calls.clear()
        leashes.vanished(5)
        leashes.vanished(5)
        assertEquals(listOf(Call.Release("five")), calls)
    }

    @Test
    fun fr3_a_task_that_appears_again_releases_the_old_surface_first() {
        leashes.appeared(5, "old", LeashPlace(0, 0, 10, 10))
        calls.clear()
        leashes.appeared(5, "new", LeashPlace(10, 20, 30, 40))
        assertEquals(listOf(Call.Release("old"), Call.Place("new", 10, 20, 30, 40)), calls)
    }
}
