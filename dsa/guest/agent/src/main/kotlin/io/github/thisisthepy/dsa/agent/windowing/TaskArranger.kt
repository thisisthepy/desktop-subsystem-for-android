package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot

/**
 * Keeps the guest's app tasks from overlapping, by moving them on the guest display, and
 * gives each task the size and density its host window asks for.
 *
 * Android's freeform launch puts each new task at the same default place, so two apps
 * started one after the other cover each other exactly. Each time the agent reads the
 * tasks, the arranger plans where every app task has to be ([TaskPlacement.plan]) and asks
 * the window manager to move the ones that are in the wrong place.
 *
 * The host asks for a task's size when the user resizes its window ([resize]), and for its
 * density when the window moves to a monitor of another scale ([setDensity]). Only the
 * latest of each counts. The task keeps its top left corner, within the display, and takes
 * the new size, never less than the smallest size the framework gives it; if it then overlaps
 * another task, the usual plan moves whichever of the two is newer. A size and a density asked
 * for together are applied in one change. While a resize is under way, the next one waits for
 * it to land, so the window manager never works through a backlog of sizes the user has
 * already dragged past. It waits [RESIZE_PATIENCE_MILLIS] at most: a size that does not land
 * by then must not stop the task following the user's drag for the whole of [giveUpMillis].
 *
 * A size the host marks live is a step of a drag the user has not finished. A change of a
 * task's screen size in dp recreates its activities, which takes far longer than a frame, so
 * at the first live step the arranger takes the screen size the task's configuration has
 * then, and every step of the drag gives the task its new bounds while its configuration
 * keeps that screen size ([TaskActions.resizeTaskLive]); the app only lays itself out again.
 * The first size that is not live ends the drag, even when it is the size the task already
 * has: the task takes it and its configuration follows its bounds again
 * ([TaskActions.endLiveResize]), with the density the host last asked for, so the app is
 * reconfigured once, at its final size. The end of a drag waits its turn like any size, but
 * a live size that arrives after it never replaces it. A density that arrives during a drag
 * ends the hold as well, because a screen size held at the old density would be wrong at the
 * new one; the next live step holds the screen size the task has by then. A size that is not
 * live and comes outside a drag is applied as it always was ([TaskActions.resizeTask]).
 *
 * The window manager may apply a move or a resize later (it queues it behind a running
 * transition), and it does not always call the task stack listener. So the arranger reports
 * a task that is changing at the bounds it is going to; the agent reads the tasks again
 * every [checkIntervalMillis] while a change is under way, and [tick] counts the time. A
 * change that has not landed after [giveUpMillis] is given up: the task is reported where
 * it is, and the arranger does not try to move it again until its bounds change.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class TaskArranger(
    private val actions: TaskActions,
    /** The guest display's size; may throw. */
    private val displaySize: () -> Size,
    private val log: Log,
    private val giveUpMillis: Long = 2000,
) {
    private class Pending(val target: TaskBounds, var millisLeft: Long, val resize: Boolean)

    /** What the host last asked of a task and the arranger has not yet asked the framework. */
    private class Wanted(
        var size: Size? = null,
        var densityDpi: Int? = null,
        /** Whether the latest size is a step of a drag. */
        var live: Boolean = false,
        /** Whether a size that ends the task's drag came in; a later live size keeps it. */
        var endsDrag: Boolean = false,
    )

    private val pending = HashMap<Int, Pending>()
    private val wanted = HashMap<Int, Wanted>()

    /**
     * Tasks whose window the user is dragging, and the screen size their configuration keeps
     * meanwhile: null when the framework did not say what it was, and the task is then
     * resized plainly.
     */
    private val dragging = HashMap<Int, ScreenDp?>()

    /** The density the host last asked for each task, which the end of a drag keeps. */
    private val densities = HashMap<Int, Int>()

    /** Tasks the window manager would not move, and the bounds they stayed at. */
    private val stuck = HashMap<Int, TaskBounds>()
    private var warnedNoDisplay = false
    private var warnedNoDensity = false

    /** The display's size when the tasks were last arranged, if it could be read. */
    var display: Size? = null
        private set

    /** Whether a change has been asked for and not yet seen to land, or is waiting its turn. */
    val waiting: Boolean get() = pending.isNotEmpty() || wanted.isNotEmpty()

    /**
     * How soon the agent reads the tasks again while [waiting]: every frame while the user
     * resizes a window, so the next size goes out as soon as the last one lands.
     */
    val checkIntervalMillis: Long
        get() = if (wanted.isNotEmpty() || pending.values.any { it.resize }) RESIZE_CHECK_MILLIS else MOVE_CHECK_MILLIS

    /**
     * The host asked for the task to be [size] pixels: [live] while the user is still
     * dragging the window's edge.
     */
    fun resize(taskId: Int, size: Size, live: Boolean = false) {
        val want = wanted.getOrPut(taskId) { Wanted() }
        want.size = size
        want.live = live
        if (!live && taskId in dragging) want.endsDrag = true
    }

    /** The host asked for the task to draw at [densityDpi]. */
    fun setDensity(taskId: Int, densityDpi: Int) {
        wanted.getOrPut(taskId) { Wanted() }.densityDpi = densityDpi
    }

    /**
     * Applies the sizes the host asked for, moves the [apps] that overlap one another or lie
     * outside the display, and returns the bounds to report for each: where the task is, or
     * where it is going.
     */
    fun arrange(apps: List<TaskSnapshot>): Map<Int, TaskBounds> {
        val current = LinkedHashMap<Int, TaskBounds>()
        val screens = HashMap<Int, ScreenDp?>()
        val smallest = HashMap<Int, Size>()
        for (task in apps) {
            if (current.putIfAbsent(task.taskId, task.bounds) == null) {
                screens[task.taskId] = task.screenDp
                smallest[task.taskId] = Size(task.minWidth, task.minHeight)
            }
        }
        settle(current)
        wanted.keys.retainAll(current.keys)
        dragging.keys.retainAll(current.keys)
        densities.keys.retainAll(current.keys)

        val size = try {
            displaySize().also { warnedNoDisplay = false }
        } catch (e: Exception) {
            if (!warnedNoDisplay) log.warn("could not read the display's size; tasks are not moved apart", e)
            warnedNoDisplay = true
            null
        }
        display = size
        if (size != null) {
            val layout = LinkedHashMap(expected(current))
            val resized = LinkedHashMap<Int, Wanted>()
            for ((taskId, want) in wanted.entries.toList()) {
                // One resize at a time: the next waits for this one to land. What it asks
                // for stays in wanted meanwhile, so the end of a drag is never lost.
                val under = pending[taskId]
                if (under != null && under.resize && giveUpMillis - under.millisLeft < RESIZE_PATIENCE_MILLIS) continue
                val now = layout.getValue(taskId)
                want.size?.let { layout[taskId] = sized(now, it, size, smallest[taskId]) }
                resized[taskId] = want
                wanted.remove(taskId)
            }
            val fixed = (stuck.keys + pending.keys) - resized.keys
            val moves = TaskPlacement.plan(layout, size, fixed).associateBy { it.taskId }
            for ((taskId, want) in resized) {
                val target = moves[taskId]?.to ?: layout.getValue(taskId)
                startResize(taskId, current.getValue(taskId), target, want, screens[taskId])
            }
            for (move in moves.values) {
                if (move.taskId !in resized) start(move, size)
            }
        }
        return expected(current)
    }

    /** [millis] have passed while a change was under way. */
    fun tick(millis: Long) {
        for (change in pending.values) change.millisLeft -= millis
    }

    private fun settle(current: Map<Int, TaskBounds>) {
        pending.keys.retainAll(current.keys)
        stuck.entries.removeAll { (taskId, bounds) -> current[taskId] != bounds }
        for ((taskId, change) in pending.entries.toList()) {
            val now = current.getValue(taskId)
            if (now == change.target) {
                pending.remove(taskId)
            } else if (change.millisLeft <= 0) {
                val what = if (change.resize) "take" else "move to"
                log.warn("task $taskId did not $what ${describe(change.target)}; it stays at ${describe(now)}")
                pending.remove(taskId)
                stuck[taskId] = now
            }
        }
    }

    private fun start(move: Move, display: Size) {
        if (move.overlap > 0) {
            log.warn(
                "the ${display.width}x${display.height} display has no free place for task ${move.taskId} " +
                    "(${move.to.right - move.to.left}x${move.to.bottom - move.to.top}); it goes to " +
                    "${describe(move.to)}, where it shares the fewest pixels (${move.overlap}) with other tasks",
            )
        }
        val asked = try {
            actions.moveTask(move.taskId, move.to)
        } catch (e: Exception) {
            log.warn("moving task ${move.taskId} failed", e)
            false
        }
        if (asked) {
            log.info("moving task ${move.taskId} from ${describe(move.from)} to ${describe(move.to)}")
            pending[move.taskId] = Pending(move.to, giveUpMillis, resize = false)
        } else {
            stuck[move.taskId] = move.from
        }
    }

    /**
     * Asks the framework for [to], the bounds [want] comes to, in the way the drag calls for:
     * a live step holds the screen size [screen] the task had when the drag began, the end of
     * a drag lets the configuration follow the bounds again, and anything else is a plain
     * resize.
     */
    private fun startResize(taskId: Int, from: TaskBounds, to: TaskBounds, want: Wanted, screen: ScreenDp?) {
        want.densityDpi?.let { densities[taskId] = it }
        val endsDrag = taskId in dragging && (want.endsDrag || !want.live || want.densityDpi != null)
        var densityDpi = want.densityDpi
        val outcome = try {
            when {
                endsDrag -> {
                    dragging.remove(taskId)
                    densityDpi = densities[taskId]
                    actions.endLiveResize(taskId, to, densityDpi)
                }
                want.live && want.densityDpi == null -> {
                    val held = if (taskId in dragging) dragging[taskId] else screen.also { dragging[taskId] = it }
                    if (held != null) actions.resizeTaskLive(taskId, to, held) else actions.resizeTask(taskId, to, null)
                }
                else -> actions.resizeTask(taskId, to, densityDpi)
            }
        } catch (e: Exception) {
            log.warn("resizing task $taskId failed", e)
            ResizeOutcome.REFUSED
        }
        if (outcome == ResizeOutcome.REFUSED) {
            log.warn("the framework refused to give task $taskId ${describe(to)}; it stays at ${describe(from)}")
            stuck[taskId] = from
            return
        }
        if (outcome == ResizeOutcome.BOUNDS_ONLY && densityDpi != null && !warnedNoDensity) {
            log.warn("the framework cannot set a task's density; task $taskId draws at the display's density, not $densityDpi dpi")
            warnedNoDensity = true
        }
        stuck.remove(taskId)
        pending[taskId] = Pending(to, giveUpMillis, resize = true)
    }

    private fun expected(current: Map<Int, TaskBounds>): Map<Int, TaskBounds> =
        current.mapValues { (taskId, bounds) -> pending[taskId]?.target ?: bounds }

    private fun describe(b: TaskBounds) = "[${b.left},${b.top}][${b.right},${b.bottom}]"

    companion object {
        const val MOVE_CHECK_MILLIS = 100L

        /** About one frame at 60 Hz. */
        const val RESIZE_CHECK_MILLIS = 16L

        /** The longest a task's next size waits for its last one to land. */
        const val RESIZE_PATIENCE_MILLIS = 250L

        /**
         * [now] given [size], keeping its top left corner unless that would put it past the
         * display's edge, never larger than the display and never smaller than [smallest],
         * the size the framework would give it anyway.
         */
        fun sized(now: TaskBounds, size: Size, display: Size, smallest: Size? = null): TaskBounds {
            val maxWidth = maxOf(1, display.width)
            val maxHeight = maxOf(1, display.height)
            val width = size.width.coerceIn((smallest?.width ?: 1).coerceIn(1, maxWidth), maxWidth)
            val height = size.height.coerceIn((smallest?.height ?: 1).coerceIn(1, maxHeight), maxHeight)
            val left = if (now.isEmpty()) 0 else now.left.coerceIn(0, maxOf(0, display.width - width))
            val top = if (now.isEmpty()) 0 else now.top.coerceIn(0, maxOf(0, display.height - height))
            return TaskBounds(left, top, left + width, top + height)
        }
    }
}
