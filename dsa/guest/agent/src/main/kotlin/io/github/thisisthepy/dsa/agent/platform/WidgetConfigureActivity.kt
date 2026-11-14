package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.app.ActivityOptions
import android.appwidget.AppWidgetHost
import android.content.Context
import android.content.Intent
import android.os.Bundle
import android.view.Display
import io.github.thisisthepy.dsa.agent.widgets.WidgetSetup
import io.github.thisisthepy.dsa.agent.windowing.WINDOWING_MODE_FREEFORM
import java.util.concurrent.ConcurrentHashMap

/**
 * Runs a widget's setup when the user places it.
 *
 * The framework starts a widget's configuration activity only for an activity of its host,
 * and hands the answer back only to an activity in the same task, so this one, which shows
 * nothing, asks for it and passes the answer on. It opens at the root of a freeform task of
 * its own ([start]): in the agent's home task, which the host has no window for, the setup
 * ran where the user could not see it and ended without an answer. The setup runs above it in that task, which the agent reports as the app's
 * ([io.github.thisisthepy.dsa.agent.tasks.WidgetSetupTasks]), so the host opens it as the
 * app's window, with its name and icon, and focuses it.
 *
 * The answer is the setup's: RESULT_OK places the widget, whether the setup showed anything
 * or finished at once; any other result, Back on the setup, and a closed window cancel it.
 */
class WidgetConfigureActivity : Activity() {
    private var widgetId = 0
    private var answered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        widgetId = intent.getIntExtra(EXTRA_WIDGET_ID, 0)
        // Recreated with the setup already running: its answer still comes here.
        if (savedInstanceState != null) return
        try {
            AppWidgetHost(this, WIDGET_HOST_ID).startAppWidgetConfigureActivityForResult(this, widgetId, 0, REQUEST, null)
        } catch (e: Exception) {
            AndroidLog.warn("the setup of widget $widgetId did not start", e)
            answer(WidgetSetup.Failed("${e.javaClass.simpleName}: ${e.message}"))
            finish()
        }
    }

    @Deprecated("The framework still delivers a started activity's result here.")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST) return
        answer(WidgetSetup.ofResult(resultCode))
        finish()
    }

    override fun onDestroy() {
        // An activity that goes without an answer is a setup the user did not finish: its
        // window was closed, which removes the whole task.
        if (isFinishing) answer(WidgetSetup.Canceled)
        super.onDestroy()
    }

    private fun answer(setup: WidgetSetup) {
        if (answered) return
        answered = true
        waiting.remove(widgetId)?.invoke(setup)
    }

    companion object {
        private const val EXTRA_WIDGET_ID = "io.github.thisisthepy.dsa.agent.WIDGET_ID"
        private const val REQUEST = 1

        /**
         * The flags this activity starts with: a new task, and another one even when a setup
         * runs already, so each setup has a window of its own and none joins the agent's
         * home task, whatever the affinity.
         */
        const val LAUNCH_FLAGS = Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_MULTIPLE_TASK or
            Intent.FLAG_ACTIVITY_NO_ANIMATION

        private val waiting = ConcurrentHashMap<Int, (WidgetSetup) -> Unit>()

        /** Runs the setup of [widgetId]; [done] hears once how it ended. */
        fun start(context: Context, widgetId: Int, done: (WidgetSetup) -> Unit) {
            waiting.put(widgetId, done)?.invoke(WidgetSetup.Canceled)
            val intent = Intent(context, WidgetConfigureActivity::class.java)
                .putExtra(EXTRA_WIDGET_ID, widgetId)
                .addFlags(LAUNCH_FLAGS)
            val options = ActivityOptions.makeBasic().setLaunchDisplayId(Display.DEFAULT_DISPLAY)
            try {
                // A freeform window on the desktop, as an app's own task gets. Not in the SDK,
                // so called by reflection; without it the display's mode decides, which on
                // the desktop display is freeform too.
                ActivityOptions::class.java.getMethod("setLaunchWindowingMode", Int::class.javaPrimitiveType)
                    .invoke(options, WINDOWING_MODE_FREEFORM)
            } catch (e: ReflectiveOperationException) {
                AndroidLog.warn("could not ask for a freeform window for a widget's setup", e)
            }
            try {
                context.startActivity(intent, options.toBundle())
            } catch (e: Exception) {
                waiting.remove(widgetId)
                throw e
            }
        }
    }
}
