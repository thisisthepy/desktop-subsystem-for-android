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
    /** AXIS_TILT: radians from upright, from 0 to pi/2. 0 for anything but a pen. */
    val tilt: Float = 0f,
    /** AXIS_ORIENTATION of a pen: the way its tip points, in radians from up, clockwise. */
    val orientation: Float = 0f,
)

/**
 * A pen's tilt from the host, in degrees from upright toward the right and toward the user,
 * as the framework reads a tablet's two tilt axes: the angle from upright and the way the
 * tip points (the input reader's own conversion of a tilt X and a tilt Y).
 */
fn penTilt(tiltXDegrees: Float, tiltYDegrees: Float): Pair<Float, Float> {
    if (!tiltXDegrees.isFinite() || !tiltYDegrees.isFinite()) return 0f to 0f
    val x = Math.toRadians(tiltXDegrees.coerceIn(-90f, 90f).toDouble())
    val y = Math.toRadians(tiltYDegrees.coerceIn(-90f, 90f).toDouble())
    if (x == 0.0 && y == 0.0) return 0f to 0f
    val tilt = Math.acos(Math.cos(x) * Math.cos(y))
    val orientation = Math.atan2(-Math.sin(x), Math.sin(y))
    return tilt.toFloat() to orientation.toFloat()
}

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

/** What puts a motion or key event into the guest's input, as a real device's would be. */
interface InputInjector {
    /** The framework's uptime clock, in milliseconds, which event times are on. */
    fun now(): Long

    /** Dispatches [motion] without waiting for an app to handle it. False when refused. */
    fun inject(motion: InjectedMotion): Boolean

    /** Dispatches [key] without waiting for an app to handle it. False when refused. */
    fun injectKey(key: InjectedKey): Boolean
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
 * - The fingers on every task window are one touch of the display, as they would be on a
 *   phone's screen: a finger on a second window while one is down on the first is that
 *   touch's next pointer, and the framework splits the touch between the windows. Two touches
 *   from one device at once would make the second ACTION_DOWN cancel the first.
 * - Each finger gets the lowest pointer ID the touch does not use. The host's IDs tell its
 *   fingers apart but can be any number, and the input dispatcher refuses a motion event
 *   with a pointer ID above 31. A touch carries at most [MAX_POINTERS] fingers, as a
 *   MotionEvent does; fingers beyond that are left out.
 * - Scrolling and flinging with a finger are moves of a touch, never ACTION_SCROLL, so an
 *   app sees the drag and its speed as it would from a touchscreen.
 * - A pen's tilt is AXIS_TILT and AXIS_ORIENTATION, as the framework reads a tablet's.
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

    /** A finger or a pen that is down: the task it is on, and the pointer the framework sees. */
    private class Contact(val taskId: Int, val pointer: InjectedPointer)

    private class Touch(val source: Int, val deviceId: Int, val downTime: Long) {
        /** The pointers down, in the order they went down, by the host's pointer ID. */
        val pointers = LinkedHashMap<Int, Contact>()

        /** The pointers in the order of their IDs, as a touchscreen of the guest lists them. */
        fun ordered(): List<InjectedPointer> = pointers.values.map { it.pointer }.sortedBy { it.id }

        /** The lowest pointer ID no pointer of this touch has. */
        fun freeId(): Int {
            val used = pointers.values.mapTo(HashSet()) { it.pointer.id }
            return (0 until MAX_POINTERS).first { it !in used }
        }
    }

    private val mice = HashMap<Int, Mouse>()

    /** The touch of the display going on now, whichever task windows its fingers are on. */
    private var touch: Touch? = null
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
        touch?.let { gesture ->
            // A finger on a task that ended has nowhere to lift: the touch is called off, so
            // the next one begins cleanly.
            if (gesture.pointers.values.any { it.taskId !in taskIds }) {
                sendTouch(gesture, MotionEvent.ACTION_CANCEL, 0)
                touch = null
            }
        }
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

    private fun sendTouch(gesture: Touch, action: Int, buttons: Int) {
        inject(
            InjectedMotion(
                action = action,
                source = gesture.source,
                deviceId = gesture.deviceId,
                downTime = gesture.downTime,
                buttonState = buttons,
                actionButton = 0,
                pointers = gesture.ordered(),
            ),
        )
    }

    private fun contact(event: PointerEvent, x: Float, y: Float, source: Int, deviceId: Int, toolType: Int) {
        val taskId = event.taskId
        val pressure = event.pressure.coerceIn(0f, 1f)
        val buttons = event.buttons and STYLUS_BUTTONS
        val gesture = touch
        val (tilt, orientation) =
            if (source == InputDevice.SOURCE_STYLUS) penTilt(event.tiltX, event.tiltY) else 0f to 0f
        fun pointer(id: Int, pressure: Float) = InjectedPointer(id, toolType, x, y, pressure, tilt, orientation)
        fun hover(action: Int) {
            inject(
                InjectedMotion(
                    action = action,
                    source = source,
                    deviceId = deviceId,
                    downTime = injector.now(),
                    buttonState = buttons,
                    actionButton = 0,
                    pointers = listOf(pointer(0, 0f)),
                ),
            )
        }
        // The touch this event belongs to, and the contact it moves or lifts at its new place;
        // null when the pointer is not down.
        val held = gesture?.takeIf { it.source == source }?.let { current ->
            current.pointers[event.pointerId]?.let { contact ->
                current to Contact(contact.taskId, pointer(contact.pointer.id, pressure))
            }
        }
        when (event.action) {
            PointerAction.POINTER_ACTION_DOWN -> {
                if (gesture == null) {
                    if (penHovering.remove(taskId) == true) hover(MotionEvent.ACTION_HOVER_EXIT)
                    val started = Touch(source, deviceId, injector.now())
                    started.pointers[event.pointerId] = Contact(taskId, pointer(0, pressure))
                    touch = started
                    sendTouch(started, MotionEvent.ACTION_DOWN, buttons)
                } else {
                    // One touch comes from one device; a pen while fingers are down waits.
                    if (gesture.source != source || event.pointerId in gesture.pointers) return
                    if (gesture.pointers.size >= MAX_POINTERS) return
                    gesture.pointers[event.pointerId] =
                        Contact(taskId, pointer(gesture.freeId(), pressure))
                    sendTouch(gesture, pointerAction(MotionEvent.ACTION_POINTER_DOWN, gesture, event.pointerId), buttons)
                }
            }
            PointerAction.POINTER_ACTION_MOVE -> {
                val (current, contact) = held ?: return
                current.pointers[event.pointerId] = contact
                sendTouch(current, MotionEvent.ACTION_MOVE, buttons)
            }
            PointerAction.POINTER_ACTION_UP -> {
                val (current, contact) = held ?: return
                current.pointers[event.pointerId] = contact
                if (current.pointers.size == 1) {
                    sendTouch(current, MotionEvent.ACTION_UP, buttons)
                    touch = null
                } else {
                    sendTouch(current, pointerAction(MotionEvent.ACTION_POINTER_UP, current, event.pointerId), buttons)
                    current.pointers.remove(event.pointerId)
                }
            }
            PointerAction.POINTER_ACTION_CANCEL -> {
                val (current, _) = held ?: return
                sendTouch(current, MotionEvent.ACTION_CANCEL, buttons)
                touch = null
            }
            PointerAction.POINTER_ACTION_HOVER_MOVE -> {
                if (gesture != null || source != InputDevice.SOURCE_STYLUS) return
                if (penHovering[taskId] != true) {
                    penHovering[taskId] = true
                    hover(MotionEvent.ACTION_HOVER_ENTER)
                }
                hover(MotionEvent.ACTION_HOVER_MOVE)
            }
            PointerAction.POINTER_ACTION_HOVER_EXIT -> {
                if (penHovering.remove(taskId) == true) hover(MotionEvent.ACTION_HOVER_EXIT)
            }
            // A finger scrolls by moving; only a mouse's wheel is a scroll.
            PointerAction.POINTER_ACTION_SCROLL -> Unit
            else -> log.warn("dropped pointer input with an action the agent does not know (${event.actionValue})")
        }
    }

    /** [action] for the host's pointer [pointerId], with that pointer's index in the event. */
    private fun pointerAction(action: Int, touch: Touch, pointerId: Int): Int {
        val id = touch.pointers.getValue(pointerId).pointer.id
        val index = touch.ordered().indexOfFirst { it.id == id }
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

        /** The most pointers one MotionEvent carries (the framework's MAX_POINTERS). */
        const val MAX_POINTERS = 16

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
