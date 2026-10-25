package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.Scheduler

/** The framework's windowing mode for freeform windows (WindowConfiguration). */
const val WINDOWING_MODE_FREEFORM = 5

/** The window manager settings the agent changes. Calls may throw. */
interface WindowingControl {
    /** Turns freeform window support on and lets every activity be resized. */
    fun enableFreeform()

    /** The default display's windowing mode. */
    fun displayWindowingMode(): Int

    fun setDisplayWindowingMode(mode: Int)

    /** The default display's size in pixels, as task bounds measure it. */
    fun displaySize(): Size
}

/**
 * Makes the guest's one display a freeform display, so every task gets bounds of its own and
 * the host can give each task its own window.
 *
 * The window manager learns of the freeform setting through a settings observer, so it may
 * not accept the display mode at once; the setup sets it and checks again every
 * [retryMillis], up to [attempts] times.
 */
class FreeformSetup(
    private val control: WindowingControl,
    private val scheduler: Scheduler,
    private val log: Log,
    private val retryMillis: Long = 500,
    private val attempts: Int = 20,
) {
    /** Whether the display was seen in freeform mode. */
    var freeform = false
        private set

    fun start() {
        try {
            control.enableFreeform()
        } catch (e: Exception) {
            log.warn("turning freeform windows on failed", e)
        }
        attempt(1)
    }

    private fun attempt(number: Int) {
        try {
            if (control.displayWindowingMode() == WINDOWING_MODE_FREEFORM) {
                freeform = true
                log.info("the display is a freeform display")
                return
            }
            control.setDisplayWindowingMode(WINDOWING_MODE_FREEFORM)
        } catch (e: Exception) {
            log.warn("making the display a freeform display failed (attempt $number of $attempts)", e)
        }
        if (number < attempts) {
            scheduler.postDelayed(retryMillis) { attempt(number + 1) }
        } else {
            log.warn(
                "the display is still not a freeform display after $attempts attempts; " +
                    "apps the host launches still open in freeform windows",
            )
        }
    }
}
