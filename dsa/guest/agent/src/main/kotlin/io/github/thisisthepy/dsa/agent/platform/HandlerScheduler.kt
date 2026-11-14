package io.github.thisisthepy.dsa.agent.platform

import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.agent.SlowWork

/** Runs the agent's work on one looper thread, and says when one piece of it runs long. */
class HandlerScheduler(looper: Looper) : Scheduler {
    private val handler = Handler(looper)
    private val slow = SlowWork(looper.thread.name, AndroidLog)

    override fun post(work: () -> Unit) {
        handler.post(guarded(work))
    }

    override fun postDelayed(delayMillis: Long, work: () -> Unit) {
        handler.postDelayed(guarded(work), delayMillis)
    }

    override fun postFirst(work: () -> Unit) {
        handler.postAtFrontOfQueue(guarded(work))
    }

    // An exception that escaped would end the agent's process; it is logged instead.
    private fun guarded(work: () -> Unit) = Runnable {
        val started = SystemClock.uptimeMillis()
        try {
            work()
        } catch (e: Exception) {
            AndroidLog.warn("the agent's work failed", e)
        }
        slow.ran(work, started, SystemClock.uptimeMillis())
    }
}
