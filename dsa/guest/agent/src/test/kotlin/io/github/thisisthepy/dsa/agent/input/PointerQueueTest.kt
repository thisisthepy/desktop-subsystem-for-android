package io.github.thisisthepy.dsa.agent.input

import io.github.thisisthepy.dsa.agent.FakeScheduler
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import io.github.thisisthepy.dsa.control.v1.PointerTool
import org.junit.Assert.assertEquals
import org.junit.Test

class PointerQueueTest {
    private val scheduler = FakeScheduler()
    private val handled = mutableListOf<PointerEvent>()
    private val waits = mutableListOf<Long>()
    private var clock = 0L
    private val queue = PointerQueue(scheduler, { clock }) { event, waited ->
        handled += event
        waits += waited
    }

    private fun event(action: PointerAction, x: Float = 0f, pointer: Int = 0, task: Int = 1, buttons: Int = 0) =
        PointerEvent.newBuilder()
            .setTaskId(task)
            .setPointerId(pointer)
            .setTool(PointerTool.POINTER_TOOL_TOUCH)
            .setAction(action)
            .setButtons(buttons)
            .setX(x)
            .build()

    private val move = PointerAction.POINTER_ACTION_MOVE
    private val down = PointerAction.POINTER_ACTION_DOWN
    private val up = PointerAction.POINTER_ACTION_UP

    @Test
    fun fr5_moves_that_wait_collapse_to_the_newest() {
        queue.add(event(down))
        queue.add(event(move, 1f))
        queue.add(event(move, 2f))
        queue.add(event(move, 3f))
        queue.add(event(up, 3f))
        scheduler.runDue()

        assertEquals(listOf(down, move, up), handled.map { it.action })
        assertEquals(3f, handled[1].x)
    }

    @Test
    fun fr5_a_press_and_a_release_are_never_collapsed() {
        queue.add(event(down))
        queue.add(event(up))
        queue.add(event(down))
        queue.add(event(up))
        scheduler.runDue()

        assertEquals(listOf(down, up, down, up), handled.map { it.action })
    }

    @Test
    fun fr5_a_move_does_not_jump_a_press_of_the_same_gesture() {
        queue.add(event(move, 1f))
        queue.add(event(down, pointer = 1))
        queue.add(event(move, 2f))
        scheduler.runDue()

        assertEquals(listOf(move, down, move), handled.map { it.action })
    }

    @Test
    fun fr5_each_finger_keeps_its_own_newest_move() {
        queue.add(event(move, 1f, pointer = 0))
        queue.add(event(move, 10f, pointer = 1))
        queue.add(event(move, 2f, pointer = 0))
        queue.add(event(move, 20f, pointer = 1))
        scheduler.runDue()

        assertEquals(listOf(2f, 20f), handled.map { it.x })
    }

    @Test
    fun fr5_tasks_do_not_collapse_into_each_other() {
        queue.add(event(move, 1f, task = 1))
        queue.add(event(move, 2f, task = 2))
        scheduler.runDue()

        assertEquals(listOf(1, 2), handled.map { it.taskId })
    }

    @Test
    fun fr5_a_move_with_other_buttons_keeps_its_place() {
        queue.add(event(move, 1f, buttons = 1))
        queue.add(event(move, 2f, buttons = 3))
        scheduler.runDue()

        assertEquals(listOf(1, 3), handled.map { it.buttons })
    }

    @Test
    fun fr5_events_after_a_drain_are_handled_too() {
        queue.add(event(down))
        scheduler.runDue()
        queue.add(event(up))
        scheduler.runDue()

        assertEquals(listOf(down, up), handled.map { it.action })
    }

    @Test
    fun fr5_each_event_says_how_long_it_waited() {
        clock = 100
        queue.add(event(down))
        clock = 350
        scheduler.runDue()

        assertEquals(listOf(250L), waits)
    }
}
