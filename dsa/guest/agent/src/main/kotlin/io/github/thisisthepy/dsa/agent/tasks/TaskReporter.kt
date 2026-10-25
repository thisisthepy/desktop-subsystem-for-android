package io.github.thisisthepy.dsa.agent.tasks

/**
 * Keeps the host's picture of the guest's app tasks in step with the guest.
 *
 * Each [update] compares the running tasks with what the host was last told and sends only
 * the difference: TaskUpdated for a task that is new or whose bounds, package, label or focus
 * changed, TaskRemoved for a task that is gone. A frame that could not be sent is not
 * recorded as told, so the next update tries again.
 *
 * Only app tasks are reported. The agent's own tasks, tasks of no package, and home, recents
 * and the like are left out: the host shows one window per app task and nothing else.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class TaskReporter(
    private val ownPackage: String,
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
) {
    private val told = LinkedHashMap<Int, TaskSnapshot>()
    private var stale = false

    /** The tasks the host was told about and not yet told have ended. */
    val reported: Set<Int> get() = told.keys.toSet()

    fun isReported(taskId: Int): Boolean = taskId in told

    /** Where the host was last told the task is; null for a task it was not told about. */
    fun reportedBounds(taskId: Int): TaskBounds? = told[taskId]?.bounds

    /** Whether the host gets a window for [task]. */
    fun reportable(task: TaskSnapshot): Boolean =
        task.kind == TaskKind.APP && !task.packageName.isNullOrEmpty() && task.packageName != ownPackage

    fun update(tasks: List<TaskSnapshot>) {
        val current = LinkedHashMap<Int, TaskSnapshot>()
        for (task in tasks) {
            if (reportable(task) && task.taskId !in current) current[task.taskId] = task
        }
        for (taskId in told.keys.toList()) {
            if (taskId !in current && send(Messages.taskRemoved(taskId))) told.remove(taskId)
        }
        var allSent = true
        for ((taskId, task) in current) {
            if (stale || told[taskId] != task) {
                if (send(Messages.taskUpdated(task))) told[taskId] = task else allSent = false
            }
        }
        if (allSent && told.keys.all { it in current }) stale = false
    }

    /**
     * Treats what the host was told as unknown, for a new connection: the next [update]
     * sends every app task again, and still ends the tasks that ended in the meantime.
     */
    fun resend() {
        stale = true
    }
}
