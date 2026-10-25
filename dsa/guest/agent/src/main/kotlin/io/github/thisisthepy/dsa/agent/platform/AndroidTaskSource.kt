package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityTaskManager
import android.app.TaskStackListener
import android.content.ComponentName
import android.content.Context
import android.content.pm.PackageManager
import android.graphics.Rect
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskKind
import io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot
import io.github.thisisthepy.dsa.agent.tasks.TaskSource

/**
 * The guest's running tasks, from the activity task manager.
 *
 * Changes come from a task stack listener: the framework calls it when a task is created,
 * removed, moved to the front, focused, or given a new description, and on any change to the
 * task stack. Each call only says that something changed; the agent then reads every running
 * task again. A task organizer would also report bounds, but registering one hands the
 * organizer every task's surface and its transitions, which the guest's composer must keep,
 * and only one organizer is active at a time.
 *
 * A task's bounds, focus and activity type are framework fields the SDK does not publish;
 * they are read by reflection, which platform apps may use.
 */
class AndroidTaskSource(
    private val context: Context,
    private val log: Log,
) : TaskSource {
    private val activityManager = context.getSystemService(ActivityManager::class.java)
    private val fields = TaskFields(log)

    // Held so it is not collected while the framework calls it.
    private var listener: TaskStackListener? = null

    override fun snapshot(): List<TaskSnapshot> =
        @Suppress("DEPRECATION")
        activityManager.getRunningTasks(MAX_TASKS).map(::read)

    override fun listen(onChange: () -> Unit) {
        val changes = object : TaskStackListener() {
            override fun onTaskStackChanged() = onChange()
            override fun onTaskCreated(taskId: Int, componentName: ComponentName?) = onChange()
            override fun onTaskRemoved(taskId: Int) = onChange()
            override fun onTaskMovedToFront(taskInfo: ActivityManager.RunningTaskInfo?) = onChange()
            override fun onTaskDescriptionChanged(taskInfo: ActivityManager.RunningTaskInfo?) = onChange()
            override fun onTaskDisplayChanged(taskId: Int, newDisplayId: Int) = onChange()
            override fun onTaskFocusChanged(taskId: Int, focused: Boolean) = onChange()
        }
        ActivityTaskManager.getService().registerTaskStackListener(changes)
        listener = changes
    }

    private fun read(info: ActivityManager.RunningTaskInfo): TaskSnapshot {
        val root = info.baseActivity ?: info.baseIntent?.component ?: info.topActivity
        return TaskSnapshot(
            taskId = info.taskId,
            kind = if (fields.activityType(info) == ACTIVITY_TYPE_STANDARD) TaskKind.APP else TaskKind.OTHER,
            packageName = root?.packageName,
            label = label(info, root),
            bounds = fields.bounds(info)?.let { TaskBounds(it.left, it.top, it.right, it.bottom) } ?: TaskBounds.EMPTY,
            focused = fields.focused(info),
        )
    }

    /** The task's own title, else its root activity's label, else its app's, else the package. */
    private fun label(info: ActivityManager.RunningTaskInfo, root: ComponentName?): String {
        info.taskDescription?.label?.takeIf { it.isNotEmpty() }?.let { return it }
        if (root == null) return ""
        val pm = context.packageManager
        return try {
            pm.getActivityInfo(root, 0).loadLabel(pm).toString()
        } catch (e: PackageManager.NameNotFoundException) {
            try {
                pm.getApplicationInfo(root.packageName, 0).loadLabel(pm).toString()
            } catch (e: PackageManager.NameNotFoundException) {
                root.packageName
            }
        }
    }

    private companion object {
        const val MAX_TASKS = 1000

        // WindowConfiguration.ACTIVITY_TYPE_STANDARD: an app's own task, not home or recents.
        const val ACTIVITY_TYPE_STANDARD = 1
    }
}

/**
 * The task fields the SDK does not publish: TaskInfo.configuration.windowConfiguration (bounds
 * and activity type) and TaskInfo.isFocused. A field that cannot be read is logged once and
 * read as unknown: no bounds, not focused, and not an app task, so nothing is reported that
 * the agent cannot describe.
 */
private class TaskFields(private val log: Log) {
    private val taskInfo = Class.forName("android.app.TaskInfo")
    private val configuration = field { taskInfo.getField("configuration") }
    private val windowConfiguration = field { android.content.res.Configuration::class.java.getField("windowConfiguration") }
    private val getBounds = field { Class.forName("android.app.WindowConfiguration").getMethod("getBounds") }
    private val getActivityType = field { Class.forName("android.app.WindowConfiguration").getMethod("getActivityType") }
    private val isFocused = field { taskInfo.getField("isFocused") }
    private val warned = HashSet<String>()

    fun bounds(info: ActivityManager.RunningTaskInfo): Rect? =
        read("bounds") { getBounds!!.invoke(windowConfiguration(info)) as Rect }

    fun activityType(info: ActivityManager.RunningTaskInfo): Int? =
        read("activity type") { getActivityType!!.invoke(windowConfiguration(info)) as Int }

    fun focused(info: ActivityManager.RunningTaskInfo): Boolean =
        read("focus") { isFocused!!.getBoolean(info) } ?: false

    private fun windowConfiguration(info: ActivityManager.RunningTaskInfo): Any =
        windowConfiguration!!.get(configuration!!.get(info))!!

    private fun <T> field(find: () -> T): T? = try {
        find()
    } catch (e: ReflectiveOperationException) {
        log.warn("the framework has no task field the agent reads", e)
        null
    }

    private fun <T> read(what: String, get: () -> T): T? = try {
        get()
    } catch (e: Exception) {
        if (warned.add(what)) log.warn("could not read a task's $what", e)
        null
    }
}
