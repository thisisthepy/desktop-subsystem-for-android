package io.github.thisisthepy.dsa.agent.tasks

/**
 * Which app a task belongs to, for the tasks that run a widget's setup.
 *
 * Android delivers a setup's answer only to an activity of the widget host in the same task,
 * so the agent opens each setup from an activity of its own ([setupActivity]) at the root of
 * a new freeform task, and the widget's setup activity runs above it. Read by its root, that
 * task is the agent's, and the host would never get a window for it: the user would not see
 * the setup and could not finish it. So such a task is the app's whose setup it shows, as
 * long as the setup is there, and the agent's own, unreported, before the setup starts and
 * after it ends.
 *
 * Android may put a screen of its own over the setup (the system's own package, as a
 * chooser or the proxy that starts the setup is): the task stays the app's while it does.
 *
 * Not thread safe: the agent calls it from one thread.
 */
class WidgetSetupTasks(
    private val ownPackage: String,
    /** The class name of the agent's activity that opens a widget's setup. */
    private val setupActivity: String,
) {
    private val apps = HashMap<Int, String>()

    /**
     * The package the task [taskId] belongs to: its root activity's, unless that root is the
     * agent's setup activity. [numActivities] is how many activities the task holds.
     */
    fun owner(
        taskId: Int,
        rootPackage: String?,
        rootClass: String?,
        topPackage: String?,
        numActivities: Int,
    ): String? {
        if (rootPackage != ownPackage || rootClass != setupActivity) {
            apps.remove(taskId)
            return rootPackage
        }
        if (!topPackage.isNullOrEmpty() && topPackage != ownPackage && topPackage != SYSTEM) {
            apps[taskId] = topPackage
            return topPackage
        }
        if (numActivities > 1) apps[taskId]?.let { return it }
        apps.remove(taskId)
        return ownPackage
    }

    /** Whether the task [taskId] shows a widget's setup, as last read by [owner]. */
    fun isSetup(taskId: Int): Boolean = taskId in apps

    /** Forgets every task but [taskIds], the ones running now. */
    fun retain(taskIds: Collection<Int>) {
        apps.keys.retainAll(taskIds.toSet())
    }

    private companion object {
        /** The package of Android's own screens. */
        const val SYSTEM = "android"
    }
}
