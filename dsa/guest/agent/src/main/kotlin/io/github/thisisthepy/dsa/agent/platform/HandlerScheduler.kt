package io.github.thisisthepy.dsa.agent.platform

import android.os.Handler
import android.os.Looper
import io.github.thisisthepy.dsa.agent.Scheduler

/** Runs the agent's work on one looper thread. */
class HandlerScheduler(looper: Looper) : Scheduler {
    private val handler = Handler(looper)

    override fun post(work: () -> Unit) {
        handler.post(guarded(work))
    }

    override fun postDelayed(delayMillis: Long, work: () -> Unit) {
        handler.postDelayed(guarded(work), delayMillis)
    }

    // An exception that escaped would end the agent's process; it is logged instead.
    private fun guarded(work: () -> Unit) = Runnable {
        try {
            work()
        } catch (e: Exception) {
            AndroidLog.warn("the agent's work failed", e)
        }
    }
}
