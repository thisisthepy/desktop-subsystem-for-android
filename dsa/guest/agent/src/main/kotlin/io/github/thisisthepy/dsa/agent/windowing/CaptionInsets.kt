package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds

/**
 * Gives each task a top inset as high as the title bar its host window draws over it.
 *
 * The host window's title bar is Android's status bar. The window has no title bar above the
 * app: the app's frames fill the window from its top edge, and the window draws its caption
 * over them, where a phone draws its status bar. The host says how high that band is
 * ([setHeight]); the task gets status bar and caption bar inset sources of that height along
 * its top, which its app receives as its status bar, system bar and caption bar inset (the
 * image's window manager passes a freeform window its status bar inset, which stock Android
 * drops). An app that draws edge to edge then keeps its toolbar below the band, as below a
 * phone's status bar, and draws its own status bar background under the caption, whose
 * glyphs follow what it draws there. An app that does not draw edge to edge gets the status
 * bar background Android draws for it there, in its status bar colour. Nothing an app does
 * not choose to draw under the band is covered by it.
 *
 * The framework takes the inset's frame in display pixels, so it is given again whenever the
 * task's top edge moves ([update]). The frame reaches far past the task on both sides: only
 * its overlap with the task's windows counts, so a task that only changes width needs nothing
 * new.
 *
 * An app that draws its own title bar (it asked for a transparent caption bar background, as
 * on Android's desktop windows) is told where the host draws over the band: the caption bar
 * inset then carries two bounding rectangles (WindowInsets.getBoundingRects), the icon's cell
 * at the left end and the caption buttons at the right end, as wide as the host said. Those
 * rectangles are relative to the inset's frame, so that frame is then the task's own width,
 * given again when the task's left or right edge moves. The app's controls in the rest of the
 * band ([controls]) take the pointer on the host.
 *
 * The host can only say how high the band is once it has opened the task's window, which is
 * after the task's app has started and laid out its first screen. Many apps read their status
 * bar inset when a screen is created and never again (YouTube's home screen, for one), so a
 * band that arrived only then left such a screen's toolbar under the caption for as long as
 * the screen lived, while screens opened later sat below it. A task the host has not yet said
 * anything of ([update]'s `awaiting`) is therefore given a band at once, as high as the status
 * bar the framework's resources give at the task's density
 * ([TaskActions.statusBarHeight]), which the image sets to the title bar at that density's
 * monitor scale. The host's own height replaces it when it comes, by at most a few pixels on
 * a monitor of another scale.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class CaptionInsets(
    private val actions: TaskActions,
    private val log: Log,
) {
    /** What the host said of a task's band: its height and how far its icon and buttons reach. */
    private data class Band(val height: Int, val startWidth: Int, val endWidth: Int)

    /** What a task was last given: the inset's frame and its bounding rectangles. */
    private data class Given(val frame: TaskBounds, val boundingRects: List<TaskBounds>)

    private val bands = HashMap<Int, Band>()

    /** The bands given to tasks the host has not said anything of yet. */
    private val expected = HashMap<Int, Band>()

    /** The tasks whose apps draw their own title bar. */
    private val drawing = HashSet<Int>()

    /** What each task was last given; null once its inset was removed. */
    private val applied = HashMap<Int, Given?>()
    private var warned = false

    /**
     * The host's window draws its title bar over the task's top [height] pixels, its icon's
     * cell [startWidth] pixels in from the left end and its caption buttons [endWidth] pixels
     * in from the right end.
     */
    fun setHeight(taskId: Int, height: Int, bounds: TaskBounds?, startWidth: Int = 0, endWidth: Int = 0) {
        bands[taskId] = Band(height, startWidth.coerceAtLeast(0), endWidth.coerceAtLeast(0))
        expected.remove(taskId)
        apply(taskId, bounds)
    }

    /**
     * The tasks the host was told about, where they are now, and those of them whose apps draw
     * their own title bar. Forgets the tasks that ended.
     *
     * [awaiting] holds the tasks whose windows will have a title bar, with the density each
     * draws at: one the host has not said anything of yet gets the band expected at that
     * density until the host does. A task left out (it fills the screen, or is in
     * picture-in-picture) keeps no expected band.
     */
    fun update(bounds: Map<Int, TaskBounds>, appDrawn: Set<Int> = emptySet(), awaiting: Map<Int, Int> = emptyMap()) {
        bands.keys.retainAll(bounds.keys)
        expected.keys.retainAll(bounds.keys)
        applied.keys.retainAll(bounds.keys)
        drawing.clear()
        drawing.addAll(appDrawn intersect bounds.keys)
        for (taskId in bounds.keys) {
            if (taskId in bands) continue
            val height = awaiting[taskId]?.let(actions::statusBarHeight) ?: 0
            if (height > 0) expected[taskId] = Band(height, 0, 0) else expected.remove(taskId)
        }
        for ((taskId, at) in bounds) {
            if (taskId in bands || taskId in expected || applied[taskId] != null) apply(taskId, at)
        }
    }

    /**
     * Where in the title bar band of task [taskId] at [bounds] its app has controls, relative
     * to the task's top left corner: the parts of [exclusion] (what the apps on the display
     * keep clear of system gestures, in display pixels) inside the band, between the icon's
     * cell and the caption buttons. This is how Android's own desktop windows tell an app's
     * controls in its title bar from the space that drags the window. Empty unless the app
     * draws its own title bar ([appDrawn]).
     */
    fun controls(taskId: Int, bounds: TaskBounds, appDrawn: Boolean, exclusion: List<TaskBounds>): List<TaskBounds> {
        if (!appDrawn) return emptyList()
        val band = bands[taskId] ?: return emptyList()
        return controlsIn(bounds, band.height, band.startWidth, band.endWidth, exclusion)
    }

    private fun apply(taskId: Int, bounds: TaskBounds?) {
        val band = bands[taskId] ?: expected[taskId]
        // An expected band that was withdrawn is taken away again.
        if (band == null && applied[taskId] == null) return
        if (bounds == null || bounds.isEmpty()) return
        val given = if (band != null && band.height > 0) given(bounds, band, taskId in drawing) else null
        if (taskId in applied && applied[taskId] == given) return
        if (given == null && taskId !in applied) {
            applied[taskId] = null
            return
        }
        if (actions.setCaptionInset(taskId, given?.frame, given?.boundingRects.orEmpty())) {
            applied[taskId] = given
        } else if (!warned) {
            warned = true
            log.warn("could not give task $taskId a top inset for its window's title bar; its app draws under the caption buttons")
        }
    }

    private fun given(bounds: TaskBounds, band: Band, appDrawn: Boolean): Given {
        if (!appDrawn) return Given(frameOf(bounds, band.height), emptyList())
        return Given(
            TaskBounds(bounds.left, bounds.top, bounds.right, bounds.top + band.height),
            boundingRectsOf(bounds.right - bounds.left, band.height, band.startWidth, band.endWidth),
        )
    }

    companion object {
        /** How far the frame reaches to each side: past any display. */
        const val REACH = 1 shl 20

        /** The inset's frame for a task at [bounds] under a title bar [height] pixels high. */
        fun frameOf(bounds: TaskBounds, height: Int) = TaskBounds(-REACH, bounds.top, REACH, bounds.top + height)

        /**
         * The bounding rectangles of the caption bar of a task [width] pixels wide under a band
         * [height] pixels high, relative to the band: the icon's cell [startWidth] pixels wide
         * at the left, the caption buttons [endWidth] pixels wide at the right. Either is left
         * out when it is 0 wide.
         */
        fun boundingRectsOf(width: Int, height: Int, startWidth: Int, endWidth: Int): List<TaskBounds> {
            val rects = mutableListOf<TaskBounds>()
            if (startWidth > 0) rects += TaskBounds(0, 0, minOf(startWidth, width), height)
            if (endWidth > 0) rects += TaskBounds(maxOf(0, width - endWidth), 0, width, height)
            return rects
        }

        /**
         * The parts of [exclusion] (display pixels) in the band of a task at [bounds], between
         * [startWidth] from its left end and [endWidth] from its right end, [height] pixels
         * high, relative to the task's top left corner.
         */
        fun controlsIn(
            bounds: TaskBounds,
            height: Int,
            startWidth: Int,
            endWidth: Int,
            exclusion: List<TaskBounds>,
        ): List<TaskBounds> {
            if (height <= 0) return emptyList()
            val left = bounds.left + startWidth
            val right = bounds.right - endWidth
            val top = bounds.top
            val bottom = bounds.top + height
            if (right <= left) return emptyList()
            return exclusion.mapNotNull { area ->
                val clipped = TaskBounds(
                    maxOf(area.left, left),
                    maxOf(area.top, top),
                    minOf(area.right, right),
                    minOf(area.bottom, bottom),
                )
                if (clipped.isEmpty()) {
                    null
                } else {
                    TaskBounds(
                        clipped.left - bounds.left,
                        clipped.top - bounds.top,
                        clipped.right - bounds.left,
                        clipped.bottom - bounds.top,
                    )
                }
            }
        }
    }
}
