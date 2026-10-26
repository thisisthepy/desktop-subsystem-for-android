package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityManager
import android.app.ActivityOptions
import android.app.ActivityTaskManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.content.res.Configuration
import android.graphics.Rect
import android.view.Display
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.ResizeOutcome
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.ScreenDp
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM
import java.lang.reflect.InvocationTargetException

/** Starts, ends, moves and resizes tasks through the framework. */
class AndroidTaskActions(
    private val context: Context,
    private val log: Log,
) : TaskActions {
    private var warnedNoDensity = false
    private var warnedNoScreenOverrides = false

    /** Whether this framework lacks the calls that hold a task's screen size. */
    private var noScreenOverrides = false

    override fun launchApp(packageName: String): Boolean {
        val intent = context.packageManager.getLaunchIntentForPackage(packageName) ?: return false
        intent.addFlags(Intent.FLAG_ACTIVITY_NEW_TASK)
        val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
        freeform(options)
        return try {
            context.startActivity(intent, options.toBundle())
            true
        } catch (e: ActivityNotFoundException) {
            log.warn("$packageName has no activity to start", e)
            false
        } catch (e: SecurityException) {
            log.warn("the framework refused to start $packageName", e)
            false
        }
    }

    override fun closeTask(taskId: Int): Boolean = ActivityTaskManager.getService().removeTask(taskId)

    /**
     * Moves a freeform task through IActivityTaskManager.resizeTask, the call behind
     * `cmd activity task resize`. It returns nothing: the window manager drops a request for a
     * task it cannot resize, and with shell transitions it may apply the bounds after the
     * call returns. The agent sees the result in the task's bounds.
     */
    override fun moveTask(taskId: Int, bounds: TaskBounds): Boolean = try {
        ActivityTaskManager.getService().resizeTask(
            taskId,
            Rect(bounds.left, bounds.top, bounds.right, bounds.bottom),
            RESIZE_MODE_PRESERVE_WINDOW,
        )
        true
    } catch (e: LinkageError) {
        log.warn("this framework has no resizeTask; tasks cannot be moved apart", e)
        false
    }

    /**
     * Gives the task [bounds], and its app [densityDpi] when one is asked for. A density goes
     * with the bounds in one window container transaction, so the app relays out once; if
     * that is not possible, the task still gets its bounds through [moveTask].
     */
    override fun resizeTask(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        if (densityDpi != null && applyWithDensity(taskId, bounds, densityDpi)) return ResizeOutcome.APPLIED
        return when {
            !moveTask(taskId, bounds) -> ResizeOutcome.REFUSED
            densityDpi == null -> ResizeOutcome.APPLIED
            else -> ResizeOutcome.BOUNDS_ONLY
        }
    }

    /**
     * Gives the task [bounds] while its configuration keeps [screen], in one window container
     * transaction (setBounds, setScreenSizeDp and setSmallestScreenWidthDp). The screen size in
     * dp is what an app's resources and layout are chosen by; held at [screen], only the
     * task's window configuration changes, which does not relaunch the task's activities, so
     * the app lays itself out again in the new bounds within a frame. Split screen holds a
     * task's configuration the same way while its divider is dragged. The orientation follows
     * from the screen size in dp, so it holds too. If this framework lacks the calls, the agent
     * logs it once and the task gets plain bounds through [moveTask].
     */
    override fun resizeTaskLive(taskId: Int, bounds: TaskBounds, screen: ScreenDp): ResizeOutcome {
        val pin = listOf(
            "setScreenSizeDp" to listOf(screen.widthDp, screen.heightDp),
            "setSmallestScreenWidthDp" to listOf(screen.smallestWidthDp),
        )
        if (!noScreenOverrides && applyTransaction(taskId, bounds, pin, ::warnNoScreenOverrides)) return ResizeOutcome.APPLIED
        return if (moveTask(taskId, bounds)) ResizeOutcome.BOUNDS_ONLY else ResizeOutcome.REFUSED
    }

    /**
     * Gives the task [bounds] and clears the screen size [resizeTaskLive] held, so the task's
     * configuration follows its bounds again, with [densityDpi] when one is asked for, all in
     * one window container transaction: the app is reconfigured once, at its final size and
     * density. A screen size of 0 (Configuration's SCREEN_WIDTH_DP_UNDEFINED and the others)
     * is how a transaction removes the override. If this framework lacks the calls, the task
     * was never held either, and this is [resizeTask].
     */
    override fun endLiveResize(taskId: Int, bounds: TaskBounds, densityDpi: Int?): ResizeOutcome {
        val clear = mutableListOf(
            "setScreenSizeDp" to listOf(Configuration.SCREEN_WIDTH_DP_UNDEFINED, Configuration.SCREEN_HEIGHT_DP_UNDEFINED),
            "setSmallestScreenWidthDp" to listOf(Configuration.SMALLEST_SCREEN_WIDTH_DP_UNDEFINED),
        )
        if (densityDpi != null) clear += "setDensityDpi" to listOf(densityDpi)
        if (!noScreenOverrides && applyTransaction(taskId, bounds, clear, ::warnNoScreenOverrides)) return ResizeOutcome.APPLIED
        return resizeTask(taskId, bounds, densityDpi)
    }

    /**
     * Sets the task's bounds and density in one WindowContainerTransaction, applied through
     * the window organizer controller. This is how the framework's own desktop windowing
     * gives a task a density of its own (WindowContainerTransaction.setDensityDpi): the task's
     * configuration carries the density, so each task on the one display can draw at the
     * density of its own host monitor. False when this framework lacks the calls or refused.
     */
    private fun applyWithDensity(taskId: Int, bounds: TaskBounds, densityDpi: Int): Boolean =
        applyTransaction(taskId, bounds, listOf("setDensityDpi" to listOf(densityDpi)), ::warnNoDensity)

    /**
     * Applies one WindowContainerTransaction to the task through the window organizer
     * controller: setBounds with [bounds], then each of [calls], a method that takes the
     * task's token and the given ints. Applying a transaction needs MANAGE_ACTIVITY_TASKS, not
     * a registered organizer. None of these classes is in the SDK, so they are reached by
     * reflection. False when this framework lacks them or refused; [warn] hears why.
     */
    private fun applyTransaction(
        taskId: Int,
        bounds: TaskBounds,
        calls: List<Pair<String, List<Int>>>,
        warn: (Throwable) -> Unit,
    ): Boolean {
        return try {
            val token = taskToken(taskId) ?: return false
            val tokenClass = Class.forName("android.window.WindowContainerToken")
            val transactionClass = Class.forName("android.window.WindowContainerTransaction")
            val transaction = transactionClass.getConstructor().newInstance()
            transactionClass.getMethod("setBounds", tokenClass, Rect::class.java)
                .invoke(transaction, token, Rect(bounds.left, bounds.top, bounds.right, bounds.bottom))
            for ((name, args) in calls) {
                val types = arrayOf<Class<*>>(tokenClass) + args.map { Int::class.javaPrimitiveType!! }
                transactionClass.getMethod(name, *types).invoke(transaction, token, *args.toTypedArray())
            }
            val controller = Class.forName("android.app.IActivityTaskManager")
                .getMethod("getWindowOrganizerController")
                .invoke(ActivityTaskManager.getService())
                ?: return false
            Class.forName("android.window.IWindowOrganizerController")
                .getMethod("applyTransaction", transactionClass)
                .invoke(controller, transaction)
            true
        } catch (e: ReflectiveOperationException) {
            warn(e)
            false
        } catch (e: LinkageError) {
            warn(e)
            false
        } catch (e: RuntimeException) {
            warn(e)
            false
        }
    }

    /** The task's window container token (TaskInfo.token, not in the SDK). */
    private fun taskToken(taskId: Int): Any? {
        @Suppress("DEPRECATION")
        val info = context.getSystemService(ActivityManager::class.java)
            .getRunningTasks(MAX_TASKS)
            .firstOrNull { it.taskId == taskId }
            ?: return null
        return Class.forName("android.app.TaskInfo").getField("token").get(info)
    }

    private fun warnNoDensity(error: Throwable) {
        if (warnedNoDensity) return
        warnedNoDensity = true
        log.warn("could not set a task's density through a window container transaction", error)
    }

    /**
     * Logs once that a task's screen size cannot be held. A framework that lacks the calls
     * (a missing class or method, not a refusal of one call) is not asked again, so each
     * step of a drag does not pay for the failed lookup.
     */
    private fun warnNoScreenOverrides(error: Throwable) {
        val missing = error is LinkageError || (error is ReflectiveOperationException && error !is InvocationTargetException)
        if (missing) noScreenOverrides = true
        if (warnedNoScreenOverrides) return
        warnedNoScreenOverrides = true
        log.warn(
            "could not hold a task's screen size through a window container transaction; " +
                "apps are reconfigured at each step of a resize",
            error,
        )
    }

    /**
     * Asks for a freeform window. On a freeform display that is what a new task gets anyway;
     * this covers a display the window manager has not switched yet. ActivityOptions'
     * setLaunchWindowingMode is not in the SDK, so it is called by reflection.
     */
    private fun freeform(options: ActivityOptions) {
        try {
            ActivityOptions::class.java.getMethod("setLaunchWindowingMode", Int::class.javaPrimitiveType)
                .invoke(options, WINDOWING_MODE_FREEFORM)
        } catch (e: ReflectiveOperationException) {
            log.warn("could not ask for a freeform window; the display's mode decides", e)
        }
    }

    private companion object {
        // ActivityTaskManager.RESIZE_MODE_PRESERVE_WINDOW: keep the app's window, do not
        // relaunch the activity.
        const val RESIZE_MODE_PRESERVE_WINDOW = 1

        const val MAX_TASKS = 1000
    }
}
