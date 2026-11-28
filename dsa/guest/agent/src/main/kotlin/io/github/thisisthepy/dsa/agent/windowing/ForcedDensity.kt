package io.github.thisisthepy.dsa.agent.windowing

import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions

/**
 * The tasks the agent has marked for forced system scaling.
 *
 * Android relaunches an activity whose density changes unless its manifest says it handles a
 * change of density, so an app that does not say so restarts when the host gives its task the
 * density of another monitor. A user can set such an app to System (forced) scaling; the host
 * then asks for the density with `forced`, and the agent first marks the task in the framework
 * ([TaskActions.markForcedDensity]), which then gives it the density without the relaunch. An
 * image whose framework lacks the mark says so ([available]), and the host does not ask it.
 *
 * A density asked for with `forced` is given only once the task is marked ([beforeDensity]):
 * given unmarked, it would restart the app, which is what the user chose this to avoid. Without
 * `forced` the mark the agent gave the task is taken away, so the task follows Android's own
 * handling again when the user picks another scaling.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class ForcedDensity(private val actions: TaskActions, private val log: Log) {
    private var supported: Boolean? = null
    private val marked = HashSet<Int>()

    /**
     * Whether the framework can mark a task. Asked once, the first time it is needed: the
     * framework does not change while the agent runs.
     */
    val available: Boolean
        get() = supported ?: actions.forcedDensitySupported().also { found ->
            supported = found
            log.info(
                if (found) {
                    "the framework gives a task marked for forced system scaling a new density without a relaunch"
                } else {
                    "the framework cannot give a task a new density without a relaunch; forced system scaling works as system scaling"
                },
            )
        }

    /**
     * Marks the task before it is given a density with [forced], or takes away the mark the
     * agent gave it before. Returns whether the density may be given now: false only when
     * [forced] and the task could not be marked.
     */
    fun beforeDensity(taskId: Int, forced: Boolean): Boolean {
        if (!forced) {
            if (taskId in marked) {
                if (actions.markForcedDensity(taskId, false)) {
                    marked -= taskId
                } else {
                    log.warn("could not take the forced system scaling mark from task $taskId")
                }
            }
            return true
        }
        if (taskId in marked) return true
        if (!available) {
            log.warn("did not give task $taskId a new density: the framework would relaunch it, and its app is set to forced system scaling")
            return false
        }
        if (!actions.markForcedDensity(taskId, true)) {
            log.warn("did not give task $taskId a new density: the framework refused to mark it for forced system scaling")
            return false
        }
        marked += taskId
        return true
    }

    /** Forgets the tasks that are not in [running]: a task's mark ends with it. */
    fun running(running: Set<Int>) {
        marked.retainAll(running)
    }
}
