package io.github.thisisthepy.dsa.agent.input

import android.view.InputDevice
import android.view.MotionEvent
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.control.v1.PointerAction
import io.github.thisisthepy.dsa.control.v1.PointerEvent
import io.github.thisisthepy.dsa.control.v1.PointerTool

/** One pointer of a motion event, in guest display pixels. */
data class InjectedPointer(
    val id: Int,
    /** MotionEvent.TOOL_TYPE_*. */
    val toolType: Int,
    val x: Float,
    val y: Float,
    val pressure: Float,
)

/** A motion event for the framework to dispatch, as MotionEvent.obtain takes it. */
data class InjectedMotion(
    /** MotionEvent.ACTION_*, with the pointer index for a pointer down or up. */
    val action: Int,
    /** InputDevice.SOURCE_*. */
    val source: Int,
    /** Tells the framework which stream of events this one belongs to. */
    val deviceId: Int,
    /** When the gesture began, in the framework's uptime milliseconds. */
    val downTime: Long,
    /** MotionEvent.BUTTON_* held after this event. */
    val buttonState: Int,
    /** The button a BUTTON_PRESS or BUTTON_RELEASE is about; 0 otherwise. */
    val actionButton: Int,
    val pointers: List<InjectedPointer>,
    /** For ACTION_SCROLL: AXIS_VSCROLL (positive up) and AXIS_HSCROLL (positive right). */
    val vscroll: Float = 0f,
    val hscroll: Float = 0f,
)

/** What puts a motion event into the guest's input, as a real device's would be. */
interface InputInjector {
    /** The framework's uptime clock, in milliseconds, which event times are on. */
    fun now(): Long

    /** Dispatches [motion] without waiting for an app to handle it. False when refused. */
    fun inject(motion: InjectedMotion): Boolean
}

/**
 * Turns the host's pointer events into the motion events the guest's apps receive, at the
 * task's place on the display.
 *
 * The host sends one event per pointer as it happens. Android wants more than that, and
 * this class keeps what it needs to supply it:
 *
 * - The mouse is SOURCE_MOUSE. A press is ACTION_DOWN then ACTION_BUTTON_PRESS for the
 *   button, so the right button arrives as BUTTON_SECONDARY, which the framework treats as a
 *   context click, not as a long press. A second button while one is down is only a
 *   BUTTON_PRESS. Hovering begins with ACTION_HOVER_ENTER and stops before a press.
 * - The wheel is ACTION_SCROLL with AXIS_VSCROLL and AXIS_HSCROLL at the mouse's position.
 * - Fingers are SOURCE_TOUCHSCREEN, and a pen is SOURCE_STYLUS. Every finger that is down
 *   goes in each event of the touch: the second finger is ACTION_POINTER_DOWN, a move carries
 *   them all, and lifting all but the last is ACTION_POINTER_UP.
 *
 * Positions are relative to the task, and the task's place is where the agent last told
 * the host it is, which is where the host draws it. An event for a task the host was not
 * told about, or one that does not continue a gesture this class saw begin, is dropped.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class PointerInput(
    private val injector: InputInjector,
    /** Where the host was told the task is on the display; null for a task it was not told about. */
    private val boundsOf: (Int) -> TaskBounds?,
    private val log: Log,
) {
    private class Mouse {
        var buttons = 0
        var downTime = 0L
        var down = false
        var hovering = false
    }

    private class Touch(val source: Int, val deviceId: Int, val downTime: Long) {
        /** The pointers down, in the order they went down, by the host's pointer ID. */
        val pointers = LinkedHashMap<Int, InjectedPointer>()
    }

    private val mice = HashMap<Int, Mouse>()
    private val touches = HashMap<Int, Touch>()
    private val penHovering = HashMap<Int, Boolean>()
    private val unknownTasks = HashSet<Int>()
    private var failing = false

    /** Acts on one pointer event from the host. */
    fun handle(event: PointerEvent) {
        val bounds = boundsOf(event.taskId)
        if (bounds == null) {
            if (unknownTasks.add(event.taskId)) {
                log.warn("dropped pointer input for task ${event.taskId}, which the host was never told about")
            }
            return
        }
        unknownTasks.remove(event.taskId)
        val x = bounds.left + event.x
        val y = bounds.top + event.y
        if (!x.isFinite() || !y.isFinite()) return
        when (event.tool) {
            PointerTool.POINTER_TOOL_MOUSE -> mouse(event, x, y)
            PointerTool.POINTER_TOOL_TOUCH ->
                contact(event, x, y, InputDevice.SOURCE_TOUCHSCREEN, TOUCH_DEVICE, MotionEvent.TOOL_TYPE_FINGER)
            PointerTool.POINTER_TOOL_PEN ->
                contact(event, x, y, InputDevice.SOURCE_STYLUS, PEN_DEVICE, MotionEvent.TOOL_TYPE_STYLUS)
            PointerTool.POINTER_TOOL_ERASER ->
                contact(event, x, y, InputDevice.SOURCE_STYLUS, PEN_DEVICE, MotionEvent.TOOL_TYPE_ERASER)
            else -> log.warn("dropped pointer input with a tool the agent does not know (${event.toolValue})")
        }
    }

    /** Forgets the gestures of every task not in [taskIds], as tasks end. */
    fun retainTasks(taskIds: Set<Int>) {
        mice.keys.retainAll(taskIds)
        touches.keys.retainAll(taskIds)
        penHovering.keys.retainAll(taskIds)
        unknownTasks.removeAll(taskIds)
    }

    private fun mouse(event: PointerEvent, x: Float, y: Float) {
        val mouse = mice.getOrPut(event.taskId) { Mouse() }
        val pressure = event.pressure.coerceIn(0f, 1f)
        fun send(action: Int, buttons: Int, actionButton: Int = 0, vscroll: Float = 0f, hscroll: Float = 0f) {
            inject(
                InjectedMotion(
                    action = action,
                    source = InputDevice.SOURCE_MOUSE,
                    deviceId = MOUSE_DEVICE,
                    downTime = if (mouse.down) mouse.downTime else injector.now(),
                    buttonState = buttons,
                    actionButton = actionButton,
                    pointers = listOf(InjectedPointer(0, MotionEvent.TOOL_TYPE_MOUSE, x, y, pressure)),
                    vscroll = vscroll,
                    hscroll = hscroll,
                ),
            )
        }
        fun stopHovering() {
            if (mouse.hovering) {
                mouse.hovering = false
                send(MotionEvent.ACTION_HOVER_EXIT, 0)
            }
        }
        val wanted = event.buttons and MOUSE_BUTTONS
        when (event.action) {
            PointerAction.POINTER_ACTION_HOVER_MOVE -> {
                if (mouse.down) return
                if (!mouse.hovering) {
                    mouse.hovering = true
                    send(MotionEvent.ACTION_HOVER_ENTER, 0)
                }
                send(MotionEvent.ACTION_HOVER_MOVE, 0)
            }
            PointerAction.POINTER_ACTION_HOVER_EXIT -> if (!mouse.down) stopHovering()
            PointerAction.POINTER_ACTION_DOWN -> {
                if (mouse.down) return
                stopHovering()
                mouse.down = true
                mouse.downTime = injector.now()
                mouse.buttons = wanted
                send(MotionEvent.ACTION_DOWN, wanted)
                pressButtons(0, wanted) { button, state -> send(MotionEvent.ACTION_BUTTON_PRESS, state, button) }
            }
            PointerAction.POINTER_ACTION_MOVE -> {
                if (!mouse.down) return
                val before = mouse.buttons
                if (before == wanted) {
                    send(MotionEvent.ACTION_MOVE, before)
                    return
                }
                mouse.buttons = wanted
                pressButtons(before, wanted) { button, state -> send(MotionEvent.ACTION_BUTTON_PRESS, state, button) }
                releaseButtons(before, wanted) { button, state -> send(MotionEvent.ACTION_BUTTON_RELEASE, state, button) }
            }
            PointerAction.POINTER_ACTION_UP -> {
                if (!mouse.down) return
                releaseButtons(mouse.buttons, 0) { button, state -> send(MotionEvent.ACTION_BUTTON_RELEASE, state, button) }
                send(MotionEvent.ACTION_UP, 0)
                mouse.down = false
                mouse.buttons = 0
            }
            PointerAction.POINTER_ACTION_CANCEL -> {
                if (!mouse.down) return
                send(MotionEvent.ACTION_CANCEL, 0)
                mouse.down = false
                mouse.buttons = 0
            }
            PointerAction.POINTER_ACTION_SCROLL ->
                send(MotionEvent.ACTION_SCROLL, mouse.buttons, vscroll = event.scrollY, hscroll = event.scrollX)
            else -> log.warn("dropped pointer input with an action the agent does not know (${event.actionValue})")
        }
    }

    /** Calls [press] for each button in [after] and not in [before], with the buttons held after it. */
    private inline fun pressButtons(before: Int, after: Int, press: (Int, Int) -> Unit) {
        var held = before
        for (button in BUTTONS) {
            if (after and button != 0 && before and button == 0) {
                held = held or button
                press(button, held)
            }
        }
    }

    /** Calls [release] for each button in [before] and not in [after], with the buttons held after it. */
    private inline fun releaseButtons(before: Int, after: Int, release: (Int, Int) -> Unit) {
        var held = before
        for (button in BUTTONS) {
            if (before and button != 0 && after and button == 0) {
                held = held and button.inv()
                release(button, held)
            }
        }
    }

    private fun contact(event: PointerEvent, x: Float, y: Float, source: Int, deviceId: Int, toolType: Int) {
        val taskId = event.taskId
        val pressure = event.pressure.coerceIn(0f, 1f)
        val pointer = InjectedPointer(event.pointerId, toolType, x, y, pressure)
        val buttons = event.buttons and STYLUS_BUTTONS
        val touch = touches[taskId]
        fun send(gesture: Touch, action: Int) {
            inject(
                InjectedMotion(
                    action = action,
                    source = gesture.source,
                    deviceId = gesture.deviceId,
                    downTime = gesture.downTime,
                    buttonState = buttons,
                    actionButton = 0,
                    pointers = gesture.pointers.values.toList(),
                ),
            )
        }
        fun hover(action: Int) {
            inject(
                InjectedMotion(
                    action = action,
                    source = source,
                    deviceId = deviceId,
                    downTime = injector.now(),
                    buttonState = buttons,
                    actionButton = 0,
                    pointers = listOf(pointer.copy(pressure = 0f)),
                ),
            )
        }
        when (event.action) {
            PointerAction.POINTER_ACTION_DOWN -> {
                if (touch == null) {
                    if (penHovering.remove(taskId) == true) hover(MotionEvent.ACTION_HOVER_EXIT)
                    val gesture = Touch(source, deviceId, injector.now())
                    gesture.pointers[event.pointerId] = pointer
                    touches[taskId] = gesture
                    send(gesture, MotionEvent.ACTION_DOWN)
                } else {
                    // One gesture comes from one device; a pen while fingers are down waits.
                    if (touch.source != source || event.pointerId in touch.pointers) return
                    touch.pointers[event.pointerId] = pointer
                    send(touch, pointerAction(MotionEvent.ACTION_POINTER_DOWN, touch, event.pointerId))
                }
            }
            PointerAction.POINTER_ACTION_MOVE -> {
                if (touch == null || touch.source != source || event.pointerId !in touch.pointers) return
                touch.pointers[event.pointerId] = pointer
                send(touch, MotionEvent.ACTION_MOVE)
            }
            PointerAction.POINTER_ACTION_UP -> {
                if (touch == null || touch.source != source || event.pointerId !in touch.pointers) return
                touch.pointers[event.pointerId] = pointer
                if (touch.pointers.size == 1) {
                    send(touch, MotionEvent.ACTION_UP)
                    touches.remove(taskId)
                } else {
                    send(touch, pointerAction(MotionEvent.ACTION_POINTER_UP, touch, event.pointerId))
                    touch.pointers.remove(event.pointerId)
                }
            }
            PointerAction.POINTER_ACTION_CANCEL -> {
                if (touch == null || touch.source != source) return
                send(touch, MotionEvent.ACTION_CANCEL)
                touches.remove(taskId)
            }
            PointerAction.POINTER_ACTION_HOVER_MOVE -> {
                if (touch != null || source != InputDevice.SOURCE_STYLUS) return
                if (penHovering[taskId] != true) {
                    penHovering[taskId] = true
                    hover(MotionEvent.ACTION_HOVER_ENTER)
                }
                hover(MotionEvent.ACTION_HOVER_MOVE)
            }
            PointerAction.POINTER_ACTION_HOVER_EXIT -> {
                if (penHovering.remove(taskId) == true) hover(MotionEvent.ACTION_HOVER_EXIT)
            }
            PointerAction.POINTER_ACTION_SCROLL -> Unit
            else -> log.warn("dropped pointer input with an action the agent does not know (${event.actionValue})")
        }
    }

    private fun pointerAction(action: Int, touch: Touch, pointerId: Int): Int {
        val index = touch.pointers.keys.indexOf(pointerId)
        return action or (index shl MotionEvent.ACTION_POINTER_INDEX_SHIFT)
    }

    private fun inject(motion: InjectedMotion) {
        val injected = try {
            injector.inject(motion)
        } catch (e: RuntimeException) {
            if (!failing) log.warn("the framework refused pointer input", e)
            failing = true
            return
        }
        if (!injected) {
            if (!failing) log.warn("the framework refused pointer input")
            failing = true
        } else {
            failing = false
        }
    }

    companion object {
        // The streams of events each kind of pointer makes, told apart from each other and
        // from the guest's own devices, which have small IDs.
        const val MOUSE_DEVICE = 0x44534101
        const val TOUCH_DEVICE = 0x44534102
        const val PEN_DEVICE = 0x44534103

        // The host's button bits are the framework's MotionEvent.BUTTON_* values.
        private val BUTTONS = intArrayOf(
            MotionEvent.BUTTON_PRIMARY,
            MotionEvent.BUTTON_SECONDARY,
            MotionEvent.BUTTON_TERTIARY,
            MotionEvent.BUTTON_BACK,
            MotionEvent.BUTTON_FORWARD,
        )
        private const val MOUSE_BUTTONS = MotionEvent.BUTTON_PRIMARY or MotionEvent.BUTTON_SECONDARY or
            MotionEvent.BUTTON_TERTIARY or MotionEvent.BUTTON_BACK or MotionEvent.BUTTON_FORWARD
        private const val STYLUS_BUTTONS = MotionEvent.BUTTON_STYLUS_PRIMARY or MotionEvent.BUTTON_STYLUS_SECONDARY
    }
}
