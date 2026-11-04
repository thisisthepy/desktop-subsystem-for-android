package io.github.thisisthepy.dsa.agent.input

import android.view.Display
import android.view.InputDevice
import android.view.KeyCharacterMap
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.control.v1.KeyAction
import io.github.thisisthepy.dsa.control.v1.KeyEvent

/** A key event for the framework to dispatch, as the KeyEvent constructor takes it. */
data class InjectedKey(
    /** When the key went down, in the framework's uptime milliseconds. */
    val downTime: Long,
    /** When this event happened, on the same clock. */
    val eventTime: Long,
    /** KeyEvent.ACTION_DOWN or KeyEvent.ACTION_UP. */
    val action: Int,
    /** KeyEvent.KEYCODE_*. */
    val keyCode: Int,
    val repeatCount: Int,
    /** KeyEvent.META_*. */
    val metaState: Int,
    /** KeyEvent.FLAG_*: FLAG_CANCELED on a release the user did not make. */
    val flags: Int,
    /** Which device the key came from: the framework's full virtual keyboard. */
    val deviceId: Int = KeyCharacterMap.VIRTUAL_KEYBOARD,
    /** InputDevice.SOURCE_*. */
    val source: Int = InputDevice.SOURCE_KEYBOARD,
    /** The display whose focused window receives it. */
    val displayId: Int = Display.DEFAULT_DISPLAY,
)

/**
 * Turns the host's key events into the key events the guest's apps receive, in the task
 * whose window the user typed in.
 *
 * Android sends a key to the focused window, not to a place on the display, so before the
 * first press in a task that does not hold the focus, the task is given it. That is what
 * moving between host windows with the keyboard alone needs: the guest has seen no click to
 * move its focus.
 *
 * The keys come from the framework's virtual keyboard, whose key character map is a full
 * US keyboard, so a key code with its meta state types the character a US layout gives it:
 * letters, digits and punctuation reach a text field as typed. A held key keeps the time it
 * went down, and its repeats and its release are placed after it by the time the host saw
 * them. A release for a key this class never saw go down is dropped, and a release the host
 * marks canceled carries FLAG_CANCELED, so the app lets the key go without acting on it.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class KeyInput(
    private val injector: InputInjector,
    /** Whether the host was told about the task. */
    private val isReported: (Int) -> Boolean,
    /** Whether the task holds the input focus in the guest, as last read. */
    private val isFocused: (Int) -> Boolean,
    /** Gives the task the input focus; false when the framework refused. */
    private val focus: (Int) -> Boolean,
    private val log: Log,
) {
    /** A held key: when it went down, and the key code the guest was given for it. */
    private class Held(val downTime: Long, val hostDownTime: Long, val guestKeyCode: Int)

    /** The keys held in each task, by key code. */
    private val held = HashMap<Int, HashMap<Int, Held>>()
    private val unknownTasks = HashSet<Int>()
    private var failing = false

    /** Acts on one key event from the host. */
    fun handle(event: KeyEvent) {
        val taskId = event.taskId
        if (!isReported(taskId)) {
            if (unknownTasks.add(taskId)) {
                log.warn("dropped key input for task $taskId, which the host was never told about")
            }
            return
        }
        unknownTasks.remove(taskId)
        val keyCode = event.keyCode
        if (keyCode <= 0 || keyCode > MAX_KEY_CODE) {
            log.warn("dropped key input with a key code the guest does not have ($keyCode)")
            return
        }
        val keys = held.getOrPut(taskId) { HashMap() }
        val hostTime = event.eventTimeMs.toLong() and 0xffffffffL
        when (event.action) {
            KeyAction.KEY_ACTION_DOWN -> {
                val known = keys[keyCode]
                val now = injector.now()
                val key = if (known == null) {
                    if (!isFocused(taskId) && !focus(taskId)) {
                        log.warn("could not give task $taskId the input focus; the key may reach another task")
                    }
                    Held(now, hostTime, guestKeyCode(keyCode, event.metaState)).also { keys[keyCode] = it }
                } else {
                    known
                }
                inject(
                    InjectedKey(
                        downTime = key.downTime,
                        eventTime = eventTime(key, hostTime, now),
                        action = android.view.KeyEvent.ACTION_DOWN,
                        keyCode = key.guestKeyCode,
                        repeatCount = if (known == null) 0 else event.repeatCount.coerceAtLeast(1),
                        metaState = event.metaState,
                        flags = 0,
                    ),
                )
            }
            KeyAction.KEY_ACTION_UP -> {
                val key = keys.remove(keyCode) ?: return
                inject(
                    InjectedKey(
                        downTime = key.downTime,
                        eventTime = eventTime(key, hostTime, injector.now()),
                        action = android.view.KeyEvent.ACTION_UP,
                        keyCode = key.guestKeyCode,
                        repeatCount = 0,
                        metaState = event.metaState,
                        flags = if (event.canceled) android.view.KeyEvent.FLAG_CANCELED else 0,
                    ),
                )
            }
            else -> log.warn("dropped key input with an action the agent does not know (${event.actionValue})")
        }
    }

    /**
     * The key code the guest is given for a key the host sent. Esc alone is Back: Android's
     * apps go back, close a dialog or leave a search on Back, and most never look at Escape.
     * The framework's own key map makes an Escape no app handled into Back only after the app
     * has declined it, which an injected key does not reliably reach, so the agent does it up
     * front. Esc with a modifier stays Escape, for the apps and shortcuts that use it. The
     * choice is made when the key goes down and kept until it is released, so the app sees a
     * whole press of one key.
     */
    private fun guestKeyCode(hostKeyCode: Int, metaState: Int): Int =
        if (hostKeyCode == android.view.KeyEvent.KEYCODE_ESCAPE && metaState and MODIFIERS == 0) {
            android.view.KeyEvent.KEYCODE_BACK
        } else {
            hostKeyCode
        }

    /** Forgets the keys held in every task not in [taskIds], as tasks end. */
    fun retainTasks(taskIds: Set<Int>) {
        held.keys.retainAll(taskIds)
        unknownTasks.removeAll(taskIds)
    }

    /**
     * When an event of a held key happened on the guest's clock: as long after the key went
     * down as the host saw it, and never later than now. The host's clock wraps, so only the
     * difference between its times means anything.
     */
    private fun eventTime(key: Held, hostTime: Long, now: Long): Long {
        val since = (hostTime - key.hostDownTime) and 0xffffffffL
        return minOf(key.downTime + since, now).coerceAtLeast(key.downTime)
    }

    private fun inject(key: InjectedKey) {
        val injected = try {
            injector.injectKey(key)
        } catch (e: RuntimeException) {
            if (!failing) log.warn("the framework refused key input", e)
            failing = true
            return
        }
        if (!injected) {
            if (!failing) log.warn("the framework refused key input")
            failing = true
        } else {
            failing = false
        }
    }

    private companion object {
        // The host sends key codes up to this; the framework's own end well below it.
        const val MAX_KEY_CODE = 1023

        // The modifiers that keep Esc an Escape. The locks (Caps, Num, Scroll) do not count.
        const val MODIFIERS = android.view.KeyEvent.META_SHIFT_ON or android.view.KeyEvent.META_CTRL_ON or
            android.view.KeyEvent.META_ALT_ON or android.view.KeyEvent.META_META_ON
    }
}
