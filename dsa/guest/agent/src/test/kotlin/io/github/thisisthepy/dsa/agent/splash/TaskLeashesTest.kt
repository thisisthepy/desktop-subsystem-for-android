package io.github.thisisthepy.dsa.agent.splash

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * Registering the organizer that draws starting windows makes the window manager stop
 * placing every task's surface: an organized task's surface position and crop are left to
 * the organizer. So the agent places each task's surface where the window manager would.
 */
class TaskLeashesTest {
    private sealed interface Call {
        data class Place(val leash: String, val x: Int, val y: Int, val clearCrop: Boolean) : Call
        data class Release(val leash: String) : Call
    }

    private val calls = mutableListOf<Call>()
    private val leashes = TaskLeashes(object : LeashSurfaces<String> {
        override fun place(leash: String, x: Int, y: Int, clearCrop: Boolean) {
            calls += Call.Place(leash, x, y, clearCrop)
        }

        override fun release(leash: String) {
            calls += Call.Release(leash)
        }
    })

    @Test
    fun fr3_an_organized_task_surface_stays_where_its_bounds_are() {
        leashes.appeared(5, "five", 686, 53)
        assertEquals(listOf(Call.Place("five", 686, 53, clearCrop = true)), calls)
    }

    @Test
    fun fr3_a_moved_task_surface_follows_its_bounds() {
        leashes.appeared(5, "five", 686, 53)
        calls.clear()
        leashes.changed(5, 0, 0)
        assertEquals(listOf(Call.Place("five", 0, 0, clearCrop = false)), calls)
    }

    @Test
    fun fr3_a_change_that_does_not_move_the_task_leaves_its_surface_alone() {
        leashes.appeared(5, "five", 686, 53)
        calls.clear()
        leashes.changed(5, 686, 53)
        assertEquals(emptyList<Call>(), calls)
    }

    @Test
    fun fr3_a_change_for_an_unknown_task_is_ignored() {
        leashes.changed(9, 1, 2)
        assertEquals(emptyList<Call>(), calls)
    }

    @Test
    fun fr3_a_task_that_ends_releases_its_surface() {
        leashes.appeared(5, "five", 0, 0)
        calls.clear()
        leashes.vanished(5)
        leashes.vanished(5)
        assertEquals(listOf(Call.Release("five")), calls)
    }

    @Test
    fun fr3_a_task_that_appears_again_releases_the_old_surface_first() {
        leashes.appeared(5, "old", 0, 0)
        calls.clear()
        leashes.appeared(5, "new", 10, 20)
        assertEquals(listOf(Call.Release("old"), Call.Place("new", 10, 20, clearCrop = true)), calls)
    }
}
