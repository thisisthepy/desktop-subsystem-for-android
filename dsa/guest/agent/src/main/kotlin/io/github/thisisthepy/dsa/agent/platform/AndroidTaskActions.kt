package io.github.thisisthepy.dsa.agent.platform

import android.app.ActivityOptions
import android.app.ActivityTaskManager
import android.content.ActivityNotFoundException
import android.content.Context
import android.content.Intent
import android.graphics.Rect
import android.view.Display
import io.github.thisisthepy.dsa.agent.Log
import io.github.thisisthepy.dsa.agent.requests.TaskActions
import io.github.thisisthepy.dsa.agent.tasks.TaskBounds
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM

/** Starts and ends tasks through the framework. */
class AndroidTaskActions(
    private val context: Context,
    private val log: Log,
) : TaskActions {
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
    }
}
