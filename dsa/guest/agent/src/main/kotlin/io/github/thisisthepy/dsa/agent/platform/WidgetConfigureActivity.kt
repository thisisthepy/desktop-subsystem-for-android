package io.github.thisisthepy.dsa.agent.platform

import android.app.Activity
import android.appwidget.AppWidgetHost
import android.content.Context
import android.content.Intent
import android.os.Bundle
import java.util.concurrent.ConcurrentHashMap

/**
 * Runs a widget's setup when the user places it. The framework starts a widget's
 * configuration activity only for an activity of its host, so this one, which shows nothing,
 * asks for it and hands the answer back. The widget's setup is an activity of the widget's
 * app, so it shows as a task window like any other; this activity is the agent's own, which
 * the host is never told of.
 */
class WidgetConfigureActivity : Activity() {
    private var widgetId = 0
    private var answered = false

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        widgetId = intent.getIntExtra(EXTRA_WIDGET_ID, 0)
        if (savedInstanceState != null) return
        try {
            AppWidgetHost(this, WIDGET_HOST_ID).startAppWidgetConfigureActivityForResult(this, widgetId, 0, REQUEST, null)
        } catch (e: Exception) {
            AndroidLog.warn("the setup of widget $widgetId did not start", e)
            answer(false)
            finish()
        }
    }

    @Deprecated("The framework still delivers a started activity's result here.")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        @Suppress("DEPRECATION")
        super.onActivityResult(requestCode, resultCode, data)
        if (requestCode != REQUEST) return
        answer(resultCode == RESULT_OK)
        finish()
    }

    override fun onDestroy() {
        // An activity that goes without an answer is a setup the user did not finish.
        if (isFinishing) answer(false)
        super.onDestroy()
    }

    private fun answer(finished: Boolean) {
        if (answered) return
        answered = true
        waiting.remove(widgetId)?.invoke(finished)
    }

    companion object {
        private const val EXTRA_WIDGET_ID = "io.github.thisisthepy.dsa.agent.WIDGET_ID"
        private const val REQUEST = 1

        private val waiting = ConcurrentHashMap<Int, (Boolean) -> Unit>()

        /** Runs the setup of [widgetId]; [done] hears once whether the user finished it. */
        fun start(context: Context, widgetId: Int, done: (Boolean) -> Unit) {
            waiting.put(widgetId, done)?.invoke(false)
            val intent = Intent(context, WidgetConfigureActivity::class.java)
                .putExtra(EXTRA_WIDGET_ID, widgetId)
                .addFlags(Intent.FLAG_ACTIVITY_NEW_TASK or Intent.FLAG_ACTIVITY_NO_ANIMATION)
            try {
                context.startActivity(intent)
            } catch (e: Exception) {
                waiting.remove(widgetId)
                throw e
            }
        }
    }
}
