package io.github.thisisthepy.dsa.agent.input

import android.view.InputDevice
import android.view.MotionEvent
import io.github.thisisthepy.dsa.agent.FakeInjector
import io.github.thisisthepy.dsa.agent.RecordingLog
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import io.github.thisisthepy.dsa.control.v1.PointerTool
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test

class PointerInputTest {
    private val injector = FakeInjector()
    private val log = RecordingLog()
    private val bounds = mutableMapOf(7 to TaskBounds(100, 50, 500, 650), 8 to TaskBounds(600, 50, 1000, 650))
    private val input = PointerInput(injector, { bounds[it] }, log)

    private fun event(
        action: PointerAction,
        x: Float,
        y: Float,
        tool: PointerTool = PointerTool.POINTER_TOOL_MOUSE,
        buttons: Int = 0,
        pointerId: Int = 0,
        pressure: Float = if (buttons != 0) 1f else 0f,
        taskId: Int = 7,
    ): PointerEvent = PointerEvent.newBuilder()
        .setTaskId(taskId)
        .setAction(action)
        .setTool(tool)
        .setPointerId(pointerId)
        .setX(x)
        .setY(y)
        .setButtons(buttons)
        .setPressure(pressure)
        .build()

    private fun actions() = injector.injected.map { it.action }

    @Test
    fun fr5_a_position_relative_to_the_task_lands_at_the_tasks_place_on_the_display() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 0f, 0f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 399f, 599f))

        val (down, up) = injector.injected.filter { it.action == MotionEvent.ACTION_DOWN || it.action == MotionEvent.ACTION_UP }
        assertEquals(100f to 50f, down.pointers.single().let { it.x to it.y })
        assertEquals(499f to 649f, up.pointers.single().let { it.x to it.y })
    }

    @Test
    fun fr5_the_four_corners_of_a_task_land_on_its_corners_on_the_display() {
        for ((x, y) in listOf(0f to 0f, 399f to 0f, 0f to 599f, 399f to 599f)) {
            input.handle(event(PointerAction.POINTER_ACTION_DOWN, x, y, buttons = MotionEvent.BUTTON_PRIMARY))
            input.handle(event(PointerAction.POINTER_ACTION_UP, x, y))
        }
        val downs = injector.injected.filter { it.action == MotionEvent.ACTION_DOWN }.map { it.pointers.single() }
        assertEquals(
            listOf(100f to 50f, 499f to 50f, 100f to 649f, 499f to 649f),
            downs.map { it.x to it.y },
        )
    }

    @Test
    fun fr5_a_mouse_click_is_a_down_a_button_press_a_button_release_and_an_up() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 10f, 10f, buttons = MotionEvent.BUTTON_PRIMARY))
        injector.clock += 80
        input.handle(event(PointerAction.POINTER_ACTION_MOVE, 12f, 10f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 12f, 10f))

        assertEquals(
            listOf(
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.ACTION_MOVE,
                MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.ACTION_UP,
            ),
            actions(),
        )
        assertTrue(injector.injected.all { it.source == InputDevice.SOURCE_MOUSE })
        assertTrue(injector.injected.all { it.pointers.single().toolType == MotionEvent.TOOL_TYPE_MOUSE })
        // The whole click is one gesture, from the time it began.
        assertTrue(injector.injected.all { it.downTime == 1000L })
        assertEquals(
            listOf(MotionEvent.BUTTON_PRIMARY, MotionEvent.BUTTON_PRIMARY, MotionEvent.BUTTON_PRIMARY, 0, 0),
            injector.injected.map { it.buttonState },
        )
        assertEquals(MotionEvent.BUTTON_PRIMARY, injector.injected[1].actionButton)
        assertEquals(MotionEvent.BUTTON_PRIMARY, injector.injected[3].actionButton)
    }

    @Test
    fun fr5_a_right_click_is_the_secondary_button_not_a_long_press() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 10f, 10f, buttons = MotionEvent.BUTTON_SECONDARY))
        injector.clock += 2000
        input.handle(event(PointerAction.POINTER_ACTION_UP, 10f, 10f))

        val press = injector.injected[1]
        assertEquals(MotionEvent.ACTION_BUTTON_PRESS, press.action)
        assertEquals(MotionEvent.BUTTON_SECONDARY, press.actionButton)
        assertEquals(MotionEvent.BUTTON_SECONDARY, press.buttonState)
        assertEquals(MotionEvent.BUTTON_SECONDARY, injector.injected[0].buttonState)
        assertTrue(injector.injected.none { it.source == InputDevice.SOURCE_TOUCHSCREEN })
    }

    @Test
    fun fr5_a_second_button_while_one_is_down_is_only_a_button_press() {
        val both = MotionEvent.BUTTON_PRIMARY or MotionEvent.BUTTON_SECONDARY
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 10f, 10f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_MOVE, 10f, 10f, buttons = both))
        input.handle(event(PointerAction.POINTER_ACTION_MOVE, 10f, 10f, buttons = MotionEvent.BUTTON_SECONDARY))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 10f, 10f))

        assertEquals(
            listOf(
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.ACTION_UP,
            ),
            actions(),
        )
        assertEquals(
            listOf(
                MotionEvent.BUTTON_PRIMARY,
                MotionEvent.BUTTON_PRIMARY,
                MotionEvent.BUTTON_SECONDARY,
                MotionEvent.BUTTON_PRIMARY,
                MotionEvent.BUTTON_SECONDARY,
                0,
            ),
            injector.injected.map { it.actionButton.takeIf { button -> button != 0 } ?: it.buttonState },
        )
    }

    @Test
    fun fr5_the_mouse_hovers_enters_and_exits() {
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 5f, 5f))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 6f, 5f))
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 6f, 5f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 6f, 5f))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 7f, 5f))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_EXIT, 7f, 5f))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_EXIT, 7f, 5f))

        assertEquals(
            listOf(
                MotionEvent.ACTION_HOVER_ENTER,
                MotionEvent.ACTION_HOVER_MOVE,
                MotionEvent.ACTION_HOVER_MOVE,
                MotionEvent.ACTION_HOVER_EXIT,
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_BUTTON_PRESS,
                MotionEvent.ACTION_BUTTON_RELEASE,
                MotionEvent.ACTION_UP,
                MotionEvent.ACTION_HOVER_ENTER,
                MotionEvent.ACTION_HOVER_MOVE,
                MotionEvent.ACTION_HOVER_EXIT,
            ),
            actions(),
        )
    }

    @Test
    fun fr5_the_wheel_scrolls_at_the_mouses_place() {
        val scroll = PointerEvent.newBuilder(event(PointerAction.POINTER_ACTION_SCROLL, 20f, 30f))
            .setScrollY(-1f)
            .setScrollX(0.5f)
            .build()
        input.handle(scroll)

        val injected = injector.injected.single()
        assertEquals(MotionEvent.ACTION_SCROLL, injected.action)
        assertEquals(InputDevice.SOURCE_MOUSE, injected.source)
        assertEquals(-1f, injected.vscroll)
        assertEquals(0.5f, injected.hscroll)
        assertEquals(120f to 80f, injected.pointers.single().let { it.x to it.y })
    }

    @Test
    fun fr5_a_cancelled_mouse_gesture_is_cancelled_in_the_guest() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 10f, 10f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_CANCEL, 10f, 10f))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 10f, 10f))

        assertEquals(
            listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_BUTTON_PRESS, MotionEvent.ACTION_CANCEL),
            actions(),
        )
    }

    private fun touch(action: PointerAction, pointerId: Int, x: Float, y: Float) =
        event(action, x, y, tool = PointerTool.POINTER_TOOL_TOUCH, pointerId = pointerId, pressure = 1f)

    @Test
    fun fr5_every_finger_down_goes_in_each_event_of_a_touch() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 11, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 12, 200f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 12, 210f, 20f))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 11, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 12, 210f, 20f))

        assertEquals(
            listOf(
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_POINTER_DOWN or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                MotionEvent.ACTION_MOVE,
                MotionEvent.ACTION_POINTER_UP or (0 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                MotionEvent.ACTION_UP,
            ),
            actions(),
        )
        assertEquals(listOf(1, 2, 2, 2, 1), injector.injected.map { it.pointers.size })
        val move = injector.injected[2]
        assertEquals(listOf(11, 12), move.pointers.map { it.id })
        assertEquals(310f to 70f, move.pointers[1].let { it.x to it.y })
        assertTrue(injector.injected.all { it.source == InputDevice.SOURCE_TOUCHSCREEN })
        assertTrue(injector.injected.all { event -> event.pointers.all { it.toolType == MotionEvent.TOOL_TYPE_FINGER } })
        assertEquals(listOf(12), injector.injected.last().pointers.map { it.id })
    }

    @Test
    fun fr5_a_finger_that_was_never_down_moves_nothing() {
        input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 3, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 3, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_CANCEL, 3, 10f, 10f))

        assertTrue(injector.injected.isEmpty())
    }

    @Test
    fun fr5_a_cancelled_touch_cancels_every_finger() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 1, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 2, 20f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_CANCEL, 1, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 2, 20f, 30f))

        assertEquals(MotionEvent.ACTION_CANCEL, injector.injected.last().action)
        assertEquals(2, injector.injected.last().pointers.size)
        assertEquals(3, injector.injected.size)
    }

    @Test
    fun fr5_a_pen_is_a_stylus_with_its_pressure_buttons_and_hover() {
        val pen = PointerTool.POINTER_TOOL_PEN
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 5f, 5f, tool = pen, buttons = MotionEvent.BUTTON_STYLUS_PRIMARY))
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 5f, 5f, tool = pen, pressure = 0.25f))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 5f, 5f, tool = pen, pressure = 0.25f))
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 5f, 5f, tool = PointerTool.POINTER_TOOL_ERASER, pressure = 0.5f))

        assertEquals(
            listOf(
                MotionEvent.ACTION_HOVER_ENTER,
                MotionEvent.ACTION_HOVER_MOVE,
                MotionEvent.ACTION_HOVER_EXIT,
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_UP,
                MotionEvent.ACTION_DOWN,
            ),
            actions(),
        )
        assertTrue(injector.injected.all { it.source == InputDevice.SOURCE_STYLUS })
        assertEquals(MotionEvent.BUTTON_STYLUS_PRIMARY, injector.injected[1].buttonState)
        assertEquals(0.25f, injector.injected[3].pointers.single().pressure)
        assertEquals(MotionEvent.TOOL_TYPE_STYLUS, injector.injected[3].pointers.single().toolType)
        assertEquals(MotionEvent.TOOL_TYPE_ERASER, injector.injected[5].pointers.single().toolType)
    }

    @Test
    fun fr5_each_task_keeps_its_own_gesture() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 1f, 1f, buttons = MotionEvent.BUTTON_PRIMARY, taskId = 7))
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 1f, 1f, buttons = MotionEvent.BUTTON_PRIMARY, taskId = 8))

        val downs = injector.injected.filter { it.action == MotionEvent.ACTION_DOWN }
        assertEquals(listOf(101f, 601f), downs.map { it.pointers.single().x })
    }

    @Test
    fun fr5_input_for_a_task_the_host_was_not_told_about_is_dropped_and_said_once() {
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 1f, 1f, taskId = 99))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 2f, 1f, taskId = 99))

        assertTrue(injector.injected.isEmpty())
        assertEquals(1, log.lines.count { "task 99" in it })
    }

    @Test
    fun nfr5_a_refused_injection_is_said_once_and_the_next_one_still_goes() {
        injector.refuse = true
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 1f, 1f))
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 2f, 1f))
        assertEquals(1, log.lines.count { "refused" in it })

        injector.refuse = false
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 3f, 1f))
        assertEquals(MotionEvent.ACTION_HOVER_MOVE, injector.injected.single().action)
    }

    @Test
    fun fr5_a_task_that_ends_takes_its_gesture_with_it() {
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 1f, 1f, buttons = MotionEvent.BUTTON_PRIMARY))
        input.retainTasks(setOf(8))
        input.handle(event(PointerAction.POINTER_ACTION_UP, 1f, 1f))

        assertEquals(listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_BUTTON_PRESS), actions())
    }
}
