package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot

/**
 * Keeps the guest's app tasks from overlapping, by moving them on the guest display.
 *
 * Android's freeform launch puts each new task at the same default place, so two apps
 * started one after the other cover each other exactly. Each time the agent reads the
 * tasks, the arranger plans where every app task has to be ([TaskPlacement.plan]) and asks
 * the window manager to move the ones that are in the wrong place.
 *
 * The window manager may apply a move later (it queues it behind a running transition), and
 * a move does not always call the task stack listener. So the arranger reports a task that is
 * being moved at the place it is going to; the agent reads the tasks again every check
 * interval while a move is under way, and [tick] counts those checks. A move that has not
 * landed after [checks] intervals is given up: the task is reported where it is, and the
 * arranger does not try to move it again until its bounds change.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class TaskArranger(
    private val actions: TaskActions,
    /** The guest display's size; may throw. */
    private val displaySize: () -> Size,
    private val log: Log,
    private val checks: Int = 20,
) {
    private class Pending(val target: TaskBounds, var checksLeft: Int)

    private val pending = HashMap<Int, Pending>()

    /** Tasks the window manager would not move, and the bounds they stayed at. */
    private val stuck = HashMap<Int, TaskBounds>()
    private var warnedNoDisplay = false

    /** Whether a move has been asked for and not yet seen to land. */
    val waiting: Boolean get() = pending.isNotEmpty()

    /**
     * Moves the [apps] that overlap one another or lie outside the display, and returns the
     * bounds to report for each: where the task is, or where it is being moved to.
     */
    fun arrange(apps: List<TaskSnapshot>): Map<Int, TaskBounds> {
        val current = LinkedHashMap<Int, TaskBounds>()
        for (task in apps) current.putIfAbsent(task.taskId, task.bounds)
        settle(current)

        val size = try {
            displaySize().also { warnedNoDisplay = false }
        } catch (e: Exception) {
            if (!warnedNoDisplay) log.warn("could not read the display's size; tasks are not moved apart", e)
            warnedNoDisplay = true
            null
        }
        if (size != null) {
            val moves = TaskPlacement.plan(expected(current), size, fixed = stuck.keys + pending.keys)
            for (move in moves) start(move, size)
        }
        return expected(current)
    }

    /** One check interval has passed while a move was under way. */
    fun tick() {
        for (move in pending.values) move.checksLeft--
    }

    private fun settle(current: Map<Int, TaskBounds>) {
        pending.keys.retainAll(current.keys)
        stuck.entries.removeAll { (taskId, bounds) -> current[taskId] != bounds }
        for ((taskId, move) in pending.entries.toList()) {
            val now = current.getValue(taskId)
            if (now == move.target) {
                pending.remove(taskId)
            } else if (move.checksLeft <= 0) {
                log.warn("task $taskId did not move to ${describe(move.target)}; it stays at ${describe(now)}")
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
            pending[move.taskId] = Pending(move.to, checks)
        } else {
            stuck[move.taskId] = move.from
        }
    }

    private fun expected(current: Map<Int, TaskBounds>): Map<Int, TaskBounds> =
        current.mapValues { (taskId, bounds) -> pending[taskId]?.target ?: bounds }

    private fun describe(b: TaskBounds) = "[${b.left},${b.top}][${b.right},${b.bottom}]"
}
