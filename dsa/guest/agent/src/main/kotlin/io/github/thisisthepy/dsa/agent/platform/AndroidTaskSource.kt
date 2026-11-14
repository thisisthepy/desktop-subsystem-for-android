package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityTaskManager
import android.app.TaskStackListener
import android.content.ComponentName
import android.content.Context
import android.content.pm.ActivityInfo
import android.content.pm.PackageManager
import android.graphics.Rect
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.SystemBarAppearances
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.tasks.TaskKind
import io.github.thisisthepy.dsa.agent.tasks.TaskOrientation
import io.github.thisisthepy.dsa.agent.tasks.asksForFullScreen
import io.github.thisisthepy.dsa.agent.tasks.TaskSnapshot
import io.github.thisisthepy.dsa.agent.tasks.TaskSource
import io.github.thisisthepy.dsa.agent.tasks.TaskStack
import io.github.thisisthepy.dsa.agent.tasks.WidgetSetupTasks
import io.github.thisisthepy.dsa.agent.tasks.handlesDensityChange
import io.github.thisisthepy.dsa.agent.tasks.handlesFontScaleChange

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
 *
 * A task's status bar appearance is not read from its task description, which the framework
 * only updates when an activity's description changes and so goes stale while an app that
 * draws edge to edge switches screens and tabs. It comes from the window manager's live
 * reports to the status bar ([AndroidSystemBars], [SystemBarAppearances]), each of which is a
 * change like any other.
 */
class AndroidTaskSource(
    private val context: Context,
    private val log: Log,
) : TaskSource {
    private val activityManager = context.getSystemService(ActivityManager::class.java)
    private val fields = TaskFields(log)
    private val themes = FullScreenThemes(context, log)

    // Held so it is not collected while the framework calls it.
    private var listener: TaskStackListener? = null

    private val setups = WidgetSetupTasks(context.packageName, WidgetConfigureActivity::class.java.name)

    private val appearances = SystemBarAppearances()
    private val systemBars = AndroidSystemBars(log)

    // Held while the tasks are read, so two reads at once do not mix up which tasks are setups.
    override fun snapshot(): List<TaskSnapshot> = synchronized(setups) {
        @Suppress("DEPRECATION")
        val tasks = activityManager.getRunningTasks(MAX_TASKS).map(::read)
        setups.retain(tasks.map { it.taskId })
        appearances.apply(tasks)
    }

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
        // A new appearance changes no task's description, so the task stack listener does
        // not hear it: the tasks are read again for each report.
        systemBars.listen { report ->
            appearances.report(report)
            onChange()
        }
    }

    private fun read(info: ActivityManager.RunningTaskInfo): TaskSnapshot {
        val root = info.baseActivity ?: info.baseIntent?.component ?: info.topActivity
        val smallest = fields.minSize(info, context.resources.configuration.densityDpi)
        val top = activityInfo(info.topActivity ?: root)
        // A widget's setup runs above the agent's own activity, in a task that is the app's.
        val owner = setups.owner(info.taskId, root?.packageName, root?.className, info.topActivity?.packageName, info.numActivities)
        val setup = setups.isSetup(info.taskId)
        return TaskSnapshot(
            taskId = info.taskId,
            kind = if (fields.activityType(info) == ACTIVITY_TYPE_STANDARD) TaskKind.APP else TaskKind.OTHER,
            packageName = owner,
            label = label(info, if (setup) info.topActivity ?: root else root),
            bounds = fields.bounds(info)?.let { TaskBounds(it.left, it.top, it.right, it.bottom) } ?: TaskBounds.EMPTY,
            focused = fields.focused(info),
            densityDpi = fields.densityDpi(info) ?: 0,
            screenDp = fields.screenDp(info),
            minWidth = smallest?.first ?: 0,
            minHeight = smallest?.second ?: 0,
            statusBarColor = fields.statusBarColor(info),
            orientation = fields.orientation(info),
            // Full screen whichever way an app asks for it: leaving freeform for the fullscreen
            // windowing mode, hiding both its status and navigation bars, or a full screen
            // theme, which is known before the activity has a window.
            fullscreen = asksForFullScreen(
                fullscreenMode = fields.windowingMode(info) == WINDOWING_MODE_FULLSCREEN,
                requestedVisibleTypes = fields.requestedVisibleTypes(info),
                fullscreenTheme = themes.asks(top),
            ),
            pictureInPicture = fields.windowingMode(info) == WINDOWING_MODE_PINNED,
            transparentCaption = fields.transparentCaption(info),
            // Whether Back lands on a screen the user saw is the agent's to tell, from the
            // stack as it changes (BackStacks). A setup's own first screen is above the
            // agent's: Back there cancels the setup, so a setup never offers it.
            stack = if (setup) {
                null
            } else {
                TaskStack(
                    activities = info.numActivities,
                    top = info.topActivity?.flattenToShortString(),
                    topShows = !themes.drawsNothing(top),
                    visible = fields.visible(info),
                )
            },
            // Whether its manifest says it handles a change of density itself
            // (ActivityInfo.configChanges includes density): Android relaunches any other
            // activity whose density changes. False when the package manager does not know it.
            handlesDensityChange = top?.let { handlesDensityChange(it.configChanges) } ?: false,
            // The same for a change of font scale, of the top activity and of the root one
            // under it, which Android restarts too when the user goes back to it. False when
            // the package manager does not know the top activity.
            handlesFontScaleChange = top != null && handlesFontScaleChange(top.configChanges) &&
                rootHandlesFontScale(info, root),
        )
    }

    /** Whether the task's root activity, when it is not its top one, handles a change of font scale. */
    private fun rootHandlesFontScale(info: ActivityManager.RunningTaskInfo, root: ComponentName?): Boolean {
        if (root == null || info.topActivity == null || root == info.topActivity) return true
        return activityInfo(root)?.let { handlesFontScaleChange(it.configChanges) } ?: true
    }

    /**
     * The manifest entry of [activity], read each time, so an app updated under a running task
     * is read as it is now. Null when the package manager does not know the activity.
     */
    private fun activityInfo(activity: ComponentName?): ActivityInfo? {
        if (activity == null) return null
        return try {
            context.packageManager.getActivityInfo(activity, 0)
        } catch (e: PackageManager.NameNotFoundException) {
            null
        }
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

        // WindowConfiguration.WINDOWING_MODE_FULLSCREEN and WINDOWING_MODE_PINNED: on the
        // freeform display, a task the app took to full screen, and one in picture-in-picture.
        const val WINDOWING_MODE_FULLSCREEN = 1
        const val WINDOWING_MODE_PINNED = 2
    }
}

/**
 * The task fields the SDK does not publish: TaskInfo.configuration (its density and screen
 * size, and its windowConfiguration's bounds and activity type) and TaskInfo.isFocused. A
 * field that cannot be read is logged once and read as unknown: no bounds, not focused, and
 * not an app task, so nothing is reported that the agent cannot describe.
 */
private class TaskFields(private val log: Log) {
    private val taskInfo = Class.forName("android.app.TaskInfo")
    private val configuration = field { taskInfo.getField("configuration") }
    private val windowConfiguration = field { android.content.res.Configuration::class.java.getField("windowConfiguration") }
    private val getBounds = field { Class.forName("android.app.WindowConfiguration").getMethod("getBounds") }
    private val getActivityType = field { Class.forName("android.app.WindowConfiguration").getMethod("getActivityType") }
    private val getWindowingMode = field { Class.forName("android.app.WindowConfiguration").getMethod("getWindowingMode") }
    private val topActivityInfo = optional { taskInfo.getField("topActivityInfo") }
    // The insets types the task's top window asks to be visible (TaskInfo, Android 14 and
    // later; not in the SDK).
    private val requestedVisibleTypes = optional { taskInfo.getField("requestedVisibleTypes") }
    private val isFocused = field { taskInfo.getField("isFocused") }
    private val isVisible = optional { taskInfo.getField("isVisible") }
    private val minWidth = field { taskInfo.getField("minWidth") }
    private val minHeight = field { taskInfo.getField("minHeight") }
    private val defaultMinSize = field { taskInfo.getField("defaultMinSize") }
    private val taskDescription = ActivityManager.TaskDescription::class.java

    // The appearance of the system bars of the task's top opaque activity, else of its top
    // activity (TaskDescription, Android 15 and later; not in the SDK).
    private val barsAppearance = optional {
        try {
            taskDescription.getMethod("getTopOpaqueSystemBarsAppearance")
        } catch (e: NoSuchMethodException) {
            taskDescription.getMethod("getSystemBarsAppearance")
        }
    }
    private val getStatusBarColor = optional { taskDescription.getMethod("getStatusBarColor") }
    private val warned = HashSet<String>()

    fun bounds(info: ActivityManager.RunningTaskInfo): Rect? =
        read("bounds") { getBounds!!.invoke(windowConfiguration(info)) as Rect }

    fun activityType(info: ActivityManager.RunningTaskInfo): Int? =
        read("activity type") { getActivityType!!.invoke(windowConfiguration(info)) as Int }

    /** The density the task's configuration gives its app, as a host window asked for it. */
    fun densityDpi(info: ActivityManager.RunningTaskInfo): Int? =
        read("density") { (configuration!!.get(info) as android.content.res.Configuration).densityDpi }

    /**
     * The screen size the task's configuration gives its app, which the agent holds while
     * the task's window is resized. Null while any of it is undefined (0).
     */
    fun screenDp(info: ActivityManager.RunningTaskInfo): ScreenDp? =
        read("screen size") {
            val config = configuration!!.get(info) as android.content.res.Configuration
            ScreenDp(config.screenWidthDp, config.screenHeightDp, config.smallestScreenWidthDp)
        }?.takeIf { it.widthDp > 0 && it.heightDp > 0 && it.smallestWidthDp > 0 }

    /**
     * The smallest size the framework gives the task, in pixels: what its activity declares
     * (TaskInfo.minWidth and minHeight, -1 when it declares nothing), else the framework's
     * default (TaskInfo.defaultMinSize, in dp at the density of the display [displayDensityDpi]).
     */
    fun minSize(info: ActivityManager.RunningTaskInfo, displayDensityDpi: Int): Pair<Int, Int>? =
        read("smallest size") {
            val default = defaultMinSize!!.getInt(info) * displayDensityDpi / DENSITY_BASE
            val width = minWidth!!.getInt(info)
            val height = minHeight!!.getInt(info)
            Pair(if (width > 0) width else default, if (height > 0) height else default)
        }

    /**
     * Whether the task's app asked to draw its own title bar: its task description carries
     * WindowInsetsController's APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND, which is how
     * Android's own desktop windows tell (TaskInfo.isTransparentCaptionBarAppearance in the
     * window manager shell reads the same bit of the top opaque activity's appearance). False
     * when the framework does not say.
     */
    fun transparentCaption(info: ActivityManager.RunningTaskInfo): Boolean {
        val description = info.taskDescription ?: return false
        val appearance = read("caption bar appearance") { barsAppearance!!.invoke(description) as Int }
            ?: return false
        return appearance and APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND != 0
    }

    /** The status bar color the app set, as ARGB; 0 when it set none or it cannot be read. */
    fun statusBarColor(info: ActivityManager.RunningTaskInfo): Int {
        val description = info.taskDescription ?: return 0
        return read("status bar color") { getStatusBarColor!!.invoke(description) as Int } ?: 0
    }

    /** The windowing mode the task is in (WindowConfiguration.WINDOWING_MODE_*); null when unknown. */
    fun windowingMode(info: ActivityManager.RunningTaskInfo): Int? =
        read("windowing mode") { getWindowingMode!!.invoke(windowConfiguration(info)) as Int }

    /** The insets types the task asks to be visible; null when the framework does not say. */
    fun requestedVisibleTypes(info: ActivityManager.RunningTaskInfo): Int? {
        val field = requestedVisibleTypes ?: return null
        return read("visible insets") { field.getInt(info) }
    }

    /** The orientation the task's top activity locks, from its screenOrientation. */
    fun orientation(info: ActivityManager.RunningTaskInfo): TaskOrientation {
        val activity = read("top activity") { topActivityInfo?.get(info) as android.content.pm.ActivityInfo? }
            ?: return TaskOrientation.UNSPECIFIED
        return TaskOrientation.of(activity.screenOrientation)
    }

    fun focused(info: ActivityManager.RunningTaskInfo): Boolean =
        read("focus") { isFocused!!.getBoolean(info) } ?: false

    /** Whether the task is visible (TaskInfo.isVisible); true when the framework does not say. */
    fun visible(info: ActivityManager.RunningTaskInfo): Boolean {
        val field = isVisible ?: return true
        return read("visibility") { field.getBoolean(info) } ?: true
    }

    private fun windowConfiguration(info: ActivityManager.RunningTaskInfo): Any =
        windowConfiguration!!.get(configuration!!.get(info))!!

    private fun <T> field(find: () -> T): T? = try {
        find()
    } catch (e: ReflectiveOperationException) {
        log.warn("the framework has no task field the agent reads", e)
        null
    }

    /** A member only some frameworks have: missing is not worth a warning. */
    private fun <T> optional(find: () -> T): T? = try {
        find()
    } catch (e: ReflectiveOperationException) {
        null
    }

    private fun <T> read(what: String, get: () -> T): T? = try {
        get()
    } catch (e: Exception) {
        if (warned.add(what)) log.warn("could not read a task's $what", e)
        null
    }

    private companion object {
        // DisplayMetrics.DENSITY_DEFAULT: the density at which one dp is one pixel.
        const val DENSITY_BASE = 160

        // WindowInsetsController.APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND.
        const val APPEARANCE_TRANSPARENT_CAPTION_BAR_BACKGROUND = 128
    }
}
