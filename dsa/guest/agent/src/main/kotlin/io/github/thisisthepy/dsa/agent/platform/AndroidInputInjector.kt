package io.github.thisisthepy.dsa.agent.platform

import android.content.Context
import android.hardware.input.InputManager
import android.os.SystemClock
import android.view.InputEvent
import android.view.KeyEvent
import android.view.MotionEvent
import io.github.thisisthepy.dsa.agent.input.InjectedKey
import io.github.thisisthepy.dsa.agent.input.InjectedMotion
import io.github.thisisthepy.dsa.agent.input.InputInjector
import java.lang.reflect.InvocationTargetException
import java.lang.reflect.Method

/**
 * Puts motion and key events into the guest's input through InputManager.injectInputEvent, the call
 * behind `input tap`. The event goes through the input dispatcher like a real device's: to
 * the window under the pointer on the default display, which is the one freeform display.
 *
 * It needs INJECT_EVENTS, a signature permission the agent holds as a platform app.
 * injectInputEvent and MotionEvent.setActionButton are not in the SDK, and are called by
 * reflection; a platform app is exempt from the framework's limits on such calls.
 *
 * The other way in would be a uinput device. It would need /dev/uinput opened by a process
 * the SELinux policy allows, which an app is not, and a mouse there reports movement, not
 * places, so each event would need the pointer's position on the guest kept in step with
 * the host's. Injection takes the place, the source, the tool and the buttons of each event
 * as they are.
 */
class AndroidInputInjector(context: Context) : InputInjector {
    private val inputManager: InputManager = context.getSystemService(InputManager::class.java)
    private val injectInputEvent: Method = InputManager::class.java
        .getMethod("injectInputEvent", InputEvent::class.java, Int::class.javaPrimitiveType)
    private val setActionButton: Method? = try {
        MotionEvent::class.java.getMethod("setActionButton", Int::class.javaPrimitiveType)
    } catch (e: NoSuchMethodException) {
        null
    }

    override fun now(): Long = SystemClock.uptimeMillis()

    override fun inject(motion: InjectedMotion): Boolean {
        val properties = motion.pointers.map { pointer ->
            MotionEvent.PointerProperties().apply {
                id = pointer.id
                toolType = pointer.toolType
            }
        }.toTypedArray()
        val coords = motion.pointers.map { pointer ->
            MotionEvent.PointerCoords().apply {
                x = pointer.x
                y = pointer.y
                pressure = pointer.pressure
                size = 1f
                if (motion.vscroll != 0f) setAxisValue(MotionEvent.AXIS_VSCROLL, motion.vscroll)
                if (motion.hscroll != 0f) setAxisValue(MotionEvent.AXIS_HSCROLL, motion.hscroll)
            }
        }.toTypedArray()
        val event = MotionEvent.obtain(
            motion.downTime,
            now(),
            motion.action,
            properties.size,
            properties,
            coords,
            0,
            motion.buttonState,
            1f,
            1f,
            motion.deviceId,
            0,
            motion.source,
            0,
        )
        return try {
            if (motion.actionButton != 0) setActionButton?.invoke(event, motion.actionButton)
            injectInputEvent.invoke(inputManager, event, INJECT_INPUT_EVENT_MODE_ASYNC) as Boolean
        } catch (e: InvocationTargetException) {
            throw (e.cause as? RuntimeException) ?: IllegalStateException(e.cause)
        } finally {
            event.recycle()
        }
    }

    /**
     * Puts a key event into the guest's input. The input dispatcher sends it to the focused
     * window of the display it names. InputEvent.setDisplayId is not in the SDK either; on a
     * framework without it the event goes to the focused display, which is the same one.
     */
    override fun injectKey(key: InjectedKey): Boolean {
        val event = KeyEvent(
            key.downTime,
            key.eventTime,
            key.action,
            key.keyCode,
            key.repeatCount,
            KeyEvent.normalizeMetaState(key.metaState),
            key.deviceId,
            0,
            key.flags,
            key.source,
        )
        return try {
            setDisplayId?.invoke(event, key.displayId)
            injectInputEvent.invoke(inputManager, event, INJECT_INPUT_EVENT_MODE_ASYNC) as Boolean
        } catch (e: InvocationTargetException) {
            throw (e.cause as? RuntimeException) ?: IllegalStateException(e.cause)
        }
    }

    private val setDisplayId: Method? = try {
        InputEvent::class.java.getMethod("setDisplayId", Int::class.javaPrimitiveType)
    } catch (e: NoSuchMethodException) {
        null
    }

    private companion object {
        // InputManager.INJECT_INPUT_EVENT_MODE_ASYNC: return once the event is queued, without
        // waiting for an app to handle it, so a slow app never holds up the next event.
        const val INJECT_INPUT_EVENT_MODE_ASYNC = 0
    }
}
