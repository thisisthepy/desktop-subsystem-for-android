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
    fun fr5_a_hover_and_a_click_at_one_point_reach_the_same_display_point() {
        input.handle(event(PointerAction.POINTER_ACTION_HOVER_MOVE, 87f, 54f))
        input.handle(event(PointerAction.POINTER_ACTION_DOWN, 87f, 54f, buttons = MotionEvent.BUTTON_PRIMARY))

        val hover = injector.injected.first { it.action == MotionEvent.ACTION_HOVER_MOVE }
        val down = injector.injected.first { it.action == MotionEvent.ACTION_DOWN }
        assertEquals(187f to 104f, hover.pointers.single().let { it.x to it.y })
        assertEquals(hover.pointers.single().let { it.x to it.y }, down.pointers.single().let { it.x to it.y })
        assertEquals(InputDevice.SOURCE_MOUSE, hover.source)
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
        assertEquals(listOf(0, 1), move.pointers.map { it.id })
        assertEquals(310f to 70f, move.pointers[1].let { it.x to it.y })
        assertTrue(injector.injected.all { it.source == InputDevice.SOURCE_TOUCHSCREEN })
        assertTrue(injector.injected.all { event -> event.pointers.all { it.toolType == MotionEvent.TOOL_TYPE_FINGER } })
        assertEquals(listOf(1), injector.injected.last().pointers.map { it.id })
    }

    @Test
    fun fr5_ten_fingers_with_any_host_ids_get_the_pointer_ids_0_to_9() {
        // Windows numbers its contacts with large IDs; the input dispatcher takes 0 to 31.
        val hostIds = (0 until 10).map { 1500 + it * 7 }
        for ((finger, id) in hostIds.withIndex()) {
            input.handle(touch(PointerAction.POINTER_ACTION_DOWN, id, 20f + finger * 30f, 300f))
        }
        val last = injector.injected.last()
        assertEquals((0 until 10).toList(), last.pointers.map { it.id })
        assertEquals(
            listOf(MotionEvent.ACTION_DOWN) +
                (1 until 10).map { MotionEvent.ACTION_POINTER_DOWN or (it shl MotionEvent.ACTION_POINTER_INDEX_SHIFT) },
            actions(),
        )
        assertTrue(injector.injected.all { event -> event.pointers.all { it.id in 0..31 } })
        assertEquals((0 until 10).map { 120f + it * 30f to 350f }, last.pointers.map { it.x to it.y })
    }

    @Test
    fun fr5_a_lifted_fingers_pointer_id_goes_to_the_next_finger_in_order() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 900, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 901, 20f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 902, 30f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 900, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 903, 40f, 10f))

        val (lift, down) = injector.injected.takeLast(2)
        assertEquals(MotionEvent.ACTION_POINTER_UP or (0 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), lift.action)
        // The new finger takes ID 0 again, and comes first, as a touchscreen lists its pointers.
        assertEquals(MotionEvent.ACTION_POINTER_DOWN or (0 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT), down.action)
        assertEquals(listOf(0, 1, 2), down.pointers.map { it.id })
        assertEquals(listOf(140f, 120f, 130f), down.pointers.map { it.x })
    }

    @Test
    fun fr5_a_pinch_moves_both_fingers_in_each_event() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 40, 180f, 300f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 41, 220f, 300f))
        for (step in 1..3) {
            input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 40, 180f - step * 20f, 300f))
            input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 41, 220f + step * 20f, 300f))
        }
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 40, 120f, 300f))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 41, 280f, 300f))

        val moves = injector.injected.filter { it.action == MotionEvent.ACTION_MOVE }
        assertEquals(6, moves.size)
        assertTrue(moves.all { it.pointers.size == 2 })
        assertEquals(listOf(220f, 380f), moves.last().pointers.map { it.x })
        assertEquals(MotionEvent.ACTION_UP, injector.injected.last().action)
        assertTrue(injector.injected.all { it.downTime == injector.injected.first().downTime })
    }

    @Test
    fun fr5_a_finger_drag_and_fling_are_touch_moves_never_a_scroll() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 5, 200f, 500f))
        for (step in 1..5) {
            injector.clock += 8
            input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 5, 200f, 500f - step * 80f))
        }
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 5, 200f, 100f))
        // A scroll from a finger is not something a touchscreen does.
        input.handle(touch(PointerAction.POINTER_ACTION_SCROLL, 5, 200f, 100f))

        assertEquals(
            listOf(MotionEvent.ACTION_DOWN) + List(5) { MotionEvent.ACTION_MOVE } + MotionEvent.ACTION_UP,
            actions(),
        )
        assertTrue(injector.injected.all { it.source == InputDevice.SOURCE_TOUCHSCREEN && it.vscroll == 0f })
        assertEquals(1000L, injector.injected.last().downTime)
    }

    @Test
    fun fr5_a_held_finger_is_a_down_until_it_lifts_so_the_guest_sees_a_long_press() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 5, 200f, 200f))
        injector.clock += 800
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 5, 200f, 200f))

        assertEquals(listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_UP), actions())
        assertEquals(1000L, injector.injected.last().downTime)
        assertEquals(0, injector.injected.first().buttonState)
    }

    @Test
    fun fr5_fingers_on_two_task_windows_are_one_touch_of_the_display() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 60, 10f, 10f).toBuilder().setTaskId(7).build())
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 61, 10f, 10f).toBuilder().setTaskId(8).build())
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 61, 10f, 10f).toBuilder().setTaskId(8).build())
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 60, 10f, 10f).toBuilder().setTaskId(7).build())

        assertEquals(
            listOf(
                MotionEvent.ACTION_DOWN,
                MotionEvent.ACTION_POINTER_DOWN or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                MotionEvent.ACTION_POINTER_UP or (1 shl MotionEvent.ACTION_POINTER_INDEX_SHIFT),
                MotionEvent.ACTION_UP,
            ),
            actions(),
        )
        assertEquals(listOf(110f to 60f, 610f to 60f), injector.injected[1].pointers.map { it.x to it.y })
    }

    @Test
    fun fr5_a_touch_carries_at_most_16_fingers() {
        for (finger in 0 until 17) input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 100 + finger, finger * 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_MOVE, 116, 5f, 5f))

        assertEquals(16, injector.injected.size)
        assertEquals(16, injector.injected.last().pointers.size)
    }

    @Test
    fun fr5_a_task_that_ends_under_a_finger_cancels_the_touch() {
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 1, 10f, 10f))
        input.retainTasks(setOf(8))
        input.handle(touch(PointerAction.POINTER_ACTION_UP, 1, 10f, 10f))
        input.handle(touch(PointerAction.POINTER_ACTION_DOWN, 2, 10f, 10f).toBuilder().setTaskId(8).build())

        assertEquals(listOf(MotionEvent.ACTION_DOWN, MotionEvent.ACTION_CANCEL, MotionEvent.ACTION_DOWN), actions())
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
    fun fr5_a_pens_tilt_is_the_angle_from_upright_and_the_direction_it_leans() {
        val pen = PointerTool.POINTER_TOOL_PEN
        fun tilted(x: Float, y: Float) = event(PointerAction.POINTER_ACTION_HOVER_MOVE, 5f, 5f, tool = pen)
            .toBuilder().setTiltX(x).setTiltY(y).build()
        input.handle(tilted(0f, 0f))
        input.handle(tilted(30f, 0f))
        input.handle(tilted(0f, 45f))
        input.handle(tilted(0f, -45f))

        val pointers = injector.injected.drop(1).map { it.pointers.single() }
        assertEquals(0f, pointers[0].tilt)
        // Its top leaning right: 30 degrees from upright, its tip pointing left.
        assertEquals(Math.toRadians(30.0).toFloat(), pointers[1].tilt, 1e-5f)
        assertEquals((-Math.PI / 2).toFloat(), pointers[1].orientation, 1e-5f)
        // Leaning toward the user, and away from the user.
        assertEquals(Math.toRadians(45.0).toFloat(), pointers[2].tilt, 1e-5f)
        assertEquals(0f, pointers[2].orientation, 1e-5f)
        assertEquals(Math.PI.toFloat(), kotlin.math.abs(pointers[3].orientation), 1e-5f)
    }

    @Test
    fun fr5_a_finger_has_no_tilt() {
        input.handle(
            touch(PointerAction.POINTER_ACTION_DOWN, 1, 10f, 10f).toBuilder().setTiltX(40f).setTiltY(10f).build(),
        )

        assertEquals(0f, injector.injected.single().pointers.single().tilt)
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
