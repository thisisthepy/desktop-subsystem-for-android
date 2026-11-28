package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions

/**
 * Gives one app overlay window a density of its own, on the host's word (SetOverlayDensity).
 *
 * An app set to System (forced) scaling draws at the density of the monitor its task's window
 * is on, its overlays included, wherever they are. When the user drags such an overlay onto a
 * monitor of another density, the host asks for that monitor's density for the overlay's window
 * alone, and for the app's own again (0) once it is back on a monitor of the task's density.
 * The framework gives it to that window ([TaskActions.setOverlayDensity]): the app's process
 * converts the window's views and lays it out again, and its other windows keep their density.
 * An image whose framework lacks that change says so ([available]), and nothing is given.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class OverlayDensity(private val actions: TaskActions, private val log: Log) {
    private var supported: Boolean? = null

    /**
     * Whether the framework can give one overlay window a density. Asked once, the first time
     * it is needed: the framework does not change while the agent runs.
     */
    val available: Boolean
        get() = supported ?: actions.overlayDensitySupported().also { found ->
            supported = found
            log.info(
                if (found) {
                    "the framework draws an app overlay window at the density the host gives it"
                } else {
                    "the framework cannot give an app overlay window a density of its own; the host scales it instead"
                },
            )
        }

    /**
     * Gives the overlay window named [window] [densityDpi], or its app's own density with 0.
     * Returns whether the framework took it.
     */
    fun give(window: String, densityDpi: Int): Boolean {
        if (!available) return false
        return actions.setOverlayDensity(window, densityDpi)
    }

    /**
     * The density the framework says it gave the window named [window] before, 0 for none or
     * on an image without the change.
     */
    fun given(window: String): Int = if (available) actions.overlayDensity(window) else 0
}
