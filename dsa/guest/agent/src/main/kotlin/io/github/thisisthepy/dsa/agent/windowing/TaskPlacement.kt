package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.tasks.TaskBounds

/** A width and height in guest display pixels. */
data class Size(val width: Int, val height: Int)

/** Where a task goes: its top left corner, and how many pixels it shares with other tasks. */
data class Placement(val left: Int, val top: Int, val overlap: Long) {
    fun bounds(size: Size) = TaskBounds(left, top, left + size.width, top + size.height)
}

/** A task that has to move: [to] has the size the task already had. */
data class Move(val taskId: Int, val from: TaskBounds, val to: TaskBounds, val overlap: Long)

/**
 * Where app tasks sit on the guest's one freeform display.
 *
 * The hwcomposer gives each task the layers inside its bounds, and SurfaceFlinger does not
 * draw a window another one covers. Two tasks that share pixels can therefore show each
 * other's content, or nothing, in their host windows, so no two app tasks may overlap. Where
 * a task sits on the guest display is never seen: the host places the windows itself. Any
 * packing works, and a task only ever moves; its size is the app's.
 */
object TaskPlacement {
    /**
     * The place for a task of [size] on a [display] that already holds [occupied].
     *
     * It is the highest free place, then the leftmost. A free place, if there is one, can be
     * slid up and left until it touches the display's edge or another task, so the corners
     * looked at are the display's and those beside and below each task. With no free place
     * left, it is the place that shares the fewest pixels with other tasks, still inside the
     * display; a task larger than the display starts at its top left corner.
     */
    fun place(occupied: List<TaskBounds>, display: Size, size: Size): Placement {
        val maxLeft = maxOf(0, display.width - size.width)
        val maxTop = maxOf(0, display.height - size.height)
        val lefts = sortedSetOf(0, maxLeft)
        val tops = sortedSetOf(0, maxTop)
        for (other in occupied) {
            lefts += other.right.coerceIn(0, maxLeft)
            lefts += (other.left - size.width).coerceIn(0, maxLeft)
            tops += other.bottom.coerceIn(0, maxTop)
            tops += (other.top - size.height).coerceIn(0, maxTop)
        }
        var best: Placement? = null
        for (top in tops) {
            for (left in lefts) {
                val candidate = TaskBounds(left, top, left + size.width, top + size.height)
                val overlap = occupied.sumOf { overlap(candidate, it) }
                if (best == null || overlap < best.overlap) {
                    best = Placement(left, top, overlap)
                    if (overlap == 0L) return best
                }
            }
        }
        return best!!
    }

    /**
     * The moves that leave no two of [tasks] sharing pixels, by task ID.
     *
     * Older tasks (lower IDs) keep their place. Each task that lies inside the display and
     * overlaps no older task stays where it is; any other is moved to [place] among the older
     * ones. A task with no bounds yet is left alone. A task in [fixed] stays where it is
     * even if it overlaps: the window manager would not move it.
     */
    fun plan(tasks: Map<Int, TaskBounds>, display: Size, fixed: Set<Int> = emptySet()): List<Move> {
        val placed = mutableListOf<TaskBounds>()
        val moves = mutableListOf<Move>()
        for ((taskId, bounds) in tasks.toSortedMap()) {
            if (bounds.isEmpty()) continue
            if (taskId in fixed || (inside(bounds, display) && placed.none { overlap(bounds, it) > 0 })) {
                placed += bounds
                continue
            }
            val size = Size(bounds.right - bounds.left, bounds.bottom - bounds.top)
            val placement = place(placed, display, size)
            val to = placement.bounds(size)
            if (to != bounds) moves += Move(taskId, bounds, to, placement.overlap)
            placed += to
        }
        return moves
    }

    /** How many pixels [a] and [b] share. */
    fun overlap(a: TaskBounds, b: TaskBounds): Long {
        val width = minOf(a.right, b.right) - maxOf(a.left, b.left)
        val height = minOf(a.bottom, b.bottom) - maxOf(a.top, b.top)
        return if (width <= 0 || height <= 0) 0 else width.toLong() * height
    }

    private fun inside(bounds: TaskBounds, display: Size) =
        bounds.left >= 0 && bounds.top >= 0 && bounds.right <= display.width && bounds.bottom <= display.height
}
