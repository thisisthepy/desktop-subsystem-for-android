package io.github.thisisthepy.dsa.agent.tasks

import io.github.thisisthepy.dsa.agent.windowing.Size

/**
 * Keeps the host's picture of the guest's app tasks in step with the guest.
 *
 * Each [update] compares the running tasks with what the host was last told and sends only
 * the difference: TaskUpdated for a task that is new or whose bounds, package, label, focus
 * or density changed, TaskRemoved for a task that is gone. Every TaskUpdated also carries the
 * display's size, the largest a task can be; when it changes, every task is sent again. A frame that could not be sent is not
 * recorded as told, so the next update tries again.
 *
 * Before the first TaskUpdated of an app, the host is told the app's name and icon (AppInfo),
 * because the host's window for the app is a program carrying them. An app is told once while
 * it has tasks, and again once it has had none, so an updated app's new name and icon reach
 * the host with its next task.
 *
 * Only app tasks are reported. The agent's own tasks, tasks of no package, and home, recents
 * and the like are left out: the host shows one window per app task and nothing else.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class TaskReporter(
    private val ownPackage: String,
    private val apps: AppInfoSource = AppInfoSource.NONE,
    /** Sends one control frame; false when it could not be sent. */
    private val send: (ByteArray) -> Boolean,
) {
    private val told = LinkedHashMap<Int, TaskSnapshot>()
    private val toldApps = HashSet<String>()
    private var stale = false
    private var display: Size? = null

    /** The tasks the host was told about and not yet told have ended. */
    val reported: Set<Int> get() = told.keys.toSet()

    fun isReported(taskId: Int): Boolean = taskId in told

    /** Where the host was last told the task is; null for a task it was not told about. */
    fun reportedBounds(taskId: Int): TaskBounds? = told[taskId]?.bounds

    /** Whether the task held the input focus when the host was last told about it. */
    fun reportedFocused(taskId: Int): Boolean = told[taskId]?.focused == true

    /** The task that held the input focus when the host was last told about the tasks, if any. */
    fun focusedTask(): Int? = told.values.firstOrNull { it.focused }?.taskId

    /** Whether the host gets a window for [task]. */
    fun reportable(task: TaskSnapshot): Boolean =
        task.kind == TaskKind.APP && !task.packageName.isNullOrEmpty() && task.packageName != ownPackage

    fun update(tasks: List<TaskSnapshot>, display: Size? = this.display) {
        if (display != this.display) {
            this.display = display
            stale = true
        }
        val current = LinkedHashMap<Int, TaskSnapshot>()
        for (task in tasks) {
            // The host is not told a task's screen size in dp or its smallest size, so a change
            // to those alone is no news.
            if (reportable(task) && task.taskId !in current) {
                current[task.taskId] = task.copy(screenDp = null, minWidth = 0, minHeight = 0)
            }
        }
        for (taskId in told.keys.toList()) {
            if (taskId !in current && send(Messages.taskRemoved(taskId))) told.remove(taskId)
        }
        var allSent = true
        for ((taskId, task) in current) {
            if (stale || told[taskId] != task) {
                if (announce(task.packageName!!) && send(Messages.taskUpdated(task, display))) {
                    told[taskId] = task
                } else {
                    allSent = false
                }
            }
        }
        // An app with no task left is told again before its next one.
        val packages = told.values.mapNotNull { it.packageName }.toSet()
        toldApps.retainAll(packages)
        if (allSent && told.keys.all { it in current }) stale = false
    }

    /** Tells the host the app's name and icon unless it was told already. */
    private fun announce(packageName: String): Boolean {
        if (packageName in toldApps) return true
        val details = try {
            apps.appInfo(packageName)
        } catch (e: Exception) {
            null
        }
        // An app the agent cannot describe still gets its windows, under the task's label.
        if (details != null && !send(Messages.appInfo(packageName, details))) return false
        toldApps += packageName
        return true
    }

    /**
     * Treats what the host was told as unknown, for a new connection: the next [update]
     * sends every app and app task again, and still ends the tasks that ended in the
     * meantime.
     */
    fun resend() {
        stale = true
        toldApps.clear()
    }
}
