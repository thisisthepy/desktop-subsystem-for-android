package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class TaskPlacementTest {
    private val display = Size(1920, 1080)

    // Android's default freeform size for a phone app on the default guest display, and the
    // place it gives every new task (seen on a real machine for Settings and Clock alike).
    private val defaultSize = Size(548, 974)
    private val defaultPlace = TaskBounds(686, 53, 1234, 1027)

    private fun at(left: Int, top: Int, size: Size = defaultSize) =
        TaskBounds(left, top, left + size.width, top + size.height)

    private fun noTwoOverlap(bounds: Collection<TaskBounds>) {
        val list = bounds.toList()
        for (i in list.indices) {
            for (j in i + 1 until list.size) {
                assertEquals("${list[i]} and ${list[j]} overlap", 0L, TaskPlacement.overlap(list[i], list[j]))
            }
        }
    }

    private fun applied(tasks: Map<Int, TaskBounds>, moves: List<Move>): Map<Int, TaskBounds> =
        tasks + moves.associate { it.taskId to it.to }

    @Test
    fun fr3_a_task_on_an_empty_display_goes_to_the_top_left_corner() {
        assertEquals(Placement(0, 0, 0), TaskPlacement.place(emptyList(), display, defaultSize))
    }

    @Test
    fun fr3_a_new_task_goes_beside_the_others_without_overlap() {
        val placement = TaskPlacement.place(listOf(at(0, 0)), display, defaultSize)
        assertEquals(Placement(548, 0, 0), placement)
    }

    @Test
    fun fr3_a_task_goes_to_the_next_row_when_the_first_is_full() {
        val small = Size(800, 400)
        val placement = TaskPlacement.place(listOf(at(0, 0, small), at(800, 0, small)), display, small)
        assertEquals(Placement(0, 400, 0), placement)
    }

    @Test
    fun fr3_a_task_fills_the_gap_a_closed_task_left() {
        val placement = TaskPlacement.place(listOf(at(0, 0), at(1096, 0)), display, defaultSize)
        assertEquals(Placement(548, 0, 0), placement)
    }

    @Test
    fun fr3_a_task_fits_beside_a_task_that_is_not_at_the_corner() {
        val placement = TaskPlacement.place(listOf(defaultPlace), display, defaultSize)
        assertEquals(Placement(0, 0, 0), placement)
    }

    @Test
    fun fr3_three_default_size_tasks_fit_the_default_display() {
        val occupied = mutableListOf<TaskBounds>()
        repeat(3) {
            val placement = TaskPlacement.place(occupied, display, defaultSize)
            assertEquals(0L, placement.overlap)
            occupied += placement.bounds(defaultSize)
        }
        assertEquals(listOf(at(0, 0), at(548, 0), at(1096, 0)), occupied)
        assertTrue(TaskPlacement.place(occupied, display, defaultSize).overlap > 0)
    }

    @Test
    fun fr3_when_the_display_is_full_a_task_goes_where_it_overlaps_least() {
        val placement = TaskPlacement.place(listOf(at(0, 0), at(548, 0), at(1096, 0)), display, defaultSize)
        // In the bottom right corner: it shares 272 columns of 868 rows with the third task,
        // and lies inside none of them, nor they in it.
        assertEquals(Placement(1372, 106, 272L * 868), placement)
    }

    @Test
    fun fr3_a_task_larger_than_the_display_keeps_its_size_at_the_corner() {
        val huge = Size(2000, 1200)
        val placement = TaskPlacement.place(listOf(at(0, 0)), display, huge)
        assertEquals(0, placement.left)
        assertEquals(0, placement.top)
        assertEquals(TaskBounds(0, 0, 2000, 1200), placement.bounds(huge))
    }

    @Test
    fun fr3_two_tasks_launched_at_the_same_place_are_moved_apart() {
        val tasks = mapOf(4 to defaultPlace, 5 to defaultPlace)
        val moves = TaskPlacement.plan(tasks, display)
        assertEquals(listOf(Move(5, defaultPlace, at(0, 0), 0)), moves)
        noTwoOverlap(applied(tasks, moves).values)
    }

    @Test
    fun fr3_tasks_that_do_not_overlap_stay_where_they_are() {
        val tasks = mapOf(1 to at(0, 0), 2 to at(700, 50), 3 to at(1300, 100))
        assertEquals(emptyList<Move>(), TaskPlacement.plan(tasks, display))
    }

    @Test
    fun fr3_the_older_task_keeps_its_place_when_one_grows_into_another() {
        val tasks = mapOf(1 to TaskBounds(0, 0, 900, 974), 2 to at(548, 0))
        val moves = TaskPlacement.plan(tasks, display)
        assertEquals(listOf(Move(2, at(548, 0), at(900, 0), 0)), moves)
    }

    @Test
    fun fr3_a_task_partly_off_the_display_is_moved_onto_it() {
        val tasks = mapOf(1 to at(1600, 0))
        assertEquals(listOf(Move(1, at(1600, 0), at(0, 0), 0)), TaskPlacement.plan(tasks, display))
    }

    @Test
    fun fr3_a_task_the_window_manager_would_not_move_stays_and_others_avoid_it() {
        val tasks = mapOf(1 to defaultPlace, 2 to at(0, 0))
        val moves = TaskPlacement.plan(tasks + (3 to at(0, 0)), display, fixed = setOf(2))
        assertEquals(listOf(Move(3, at(0, 0), at(1234, 0), 0)), moves)
    }

    @Test
    fun fr3_a_task_without_bounds_is_left_alone() {
        val tasks = mapOf(1 to TaskBounds.EMPTY, 2 to defaultPlace)
        assertEquals(emptyList<Move>(), TaskPlacement.plan(tasks, display))
    }

    @Test
    fun fr3_many_tasks_at_one_place_end_up_apart_while_they_fit() {
        val small = Size(400, 300)
        val tasks = (1..12).associateWith { at(700, 300, small) }
        val moves = TaskPlacement.plan(tasks, display)
        noTwoOverlap(applied(tasks, moves).values)
        assertEquals(11, moves.size)
    }
}
