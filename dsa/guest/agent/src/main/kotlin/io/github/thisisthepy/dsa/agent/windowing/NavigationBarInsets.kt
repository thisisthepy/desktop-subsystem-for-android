package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.InsetGiven
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds

/**
 * Gives each task a bottom inset as high as a gesture navigation phone's navigation bar.
 *
 * The host has no Android navigation bar, but phone apps are laid out for one: their bottom
 * bars and the ends of their lists keep clear of the navigation bar inset, and with none they
 * sat on the window's bottom edge. A gesture navigation phone gives that inset 24 dp
 * (navigation_bar_height in AOSP's gesture navigation overlay), so each task gets navigation
 * bar and tappable element inset sources [HEIGHT_DP] high at the task's density along its
 * bottom edge, which its app receives as its navigation bar, system bar and tappable element
 * inset (the image's window manager passes a freeform window those, which stock Android drops).
 * Nothing is drawn there: the image makes the navigation bar background transparent, so the
 * app's own background reaches the window's bottom edge, whether it takes the inset or not.
 *
 * The band is given as soon as the host is told of the task, before its app lays out its first
 * screen: many apps read their insets when a screen is created and never again. A task that
 * fills the screen or is in picture-in-picture has no band, as it has no title bar; it gets
 * one again when it is back in a window. The band follows the task's density, so a task moved
 * to a monitor of another scale keeps 24 dp.
 *
 * Where the framework keeps the band on the task's bottom edge by itself
 * ([InsetGiven.FOLLOWS_TASK]), a resize needs nothing new. Otherwise the band is given again
 * whenever the task's bottom edge moves; its frame reaches far past the task on both sides, so
 * a change of width alone needs nothing new either.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class NavigationBarInsets(
    private val actions: TaskActions,
    private val log: Log,
) {
    /** What a task was last given: the band's height, and its frame when it does not follow the task. */
    private data class Given(val height: Int, val frame: TaskBounds?)

    private val applied = HashMap<Int, Given>()
    private var warned = false

    /**
     * The tasks the host was told about and where they are now, and of those the ones that get
     * a band ([densities], each with the density its app draws at). A task left out of
     * [densities], or whose density is not known, loses its band. Forgets the tasks that ended.
     */
    fun update(bounds: Map<Int, TaskBounds>, densities: Map<Int, Int>) {
        applied.keys.retainAll(bounds.keys)
        for ((taskId, at) in bounds) {
            if (at.isEmpty()) continue
            val height = densities[taskId]?.let { heightAt(it) } ?: 0
            val last = applied[taskId]
            if (height <= 0) {
                if (last != null) give(taskId, null, 0)
                continue
            }
            val frame = frameOf(at, height)
            if (last != null && last.height == height && (last.frame == null || last.frame == frame)) continue
            give(taskId, frame, height)
        }
    }

    private fun give(taskId: Int, frame: TaskBounds?, height: Int) {
        val given = actions.setNavigationBarInset(taskId, frame)
        if (given == InsetGiven.REFUSED) {
            if (!warned) {
                warned = true
                log.warn("could not give task $taskId a bottom inset for a navigation bar; its app's bottom bar meets the window's edge")
            }
            return
        }
        if (frame == null) {
            applied.remove(taskId)
        } else {
            applied[taskId] = Given(height, if (given == InsetGiven.FOLLOWS_TASK) null else frame)
        }
    }

    companion object {
        /** The navigation bar's height on a gesture navigation phone, in dp. */
        const val HEIGHT_DP = 24

        /** The dp Android's density counts from: one dp is one pixel at 160 dpi. */
        private const val BASE_DENSITY = 160

        /**
         * The band's height in pixels for an app drawing at [densityDpi]: [HEIGHT_DP] rounded
         * as getDimensionPixelSize rounds navigation_bar_height there. 0 for an unknown density.
         */
        fun heightAt(densityDpi: Int): Int =
            if (densityDpi <= 0) 0 else ((HEIGHT_DP * densityDpi + BASE_DENSITY / 2) / BASE_DENSITY).coerceAtLeast(1)

        /** The band's frame for a task at [bounds], [height] pixels high along its bottom edge. */
        fun frameOf(bounds: TaskBounds, height: Int) =
            TaskBounds(-CaptionInsets.REACH, bounds.bottom - height, CaptionInsets.REACH, bounds.bottom)
    }
}
