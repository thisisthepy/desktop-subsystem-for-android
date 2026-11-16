package io.github.thisisthepy.dsa.agent.platform

import android.os.Handler
import android.os.HandlerThread
import android.os.Looper
import android.os.SystemClock
import io.github.thisisthepy.dsa.agent.Scheduler
import io.github.thisisthepy.dsa.agent.SlowWork

/** Runs the agent's work on one looper thread, and says when one piece of it runs long. */
class HandlerScheduler(looper: Looper) : Scheduler {
    private val handler = Handler(looper)
    private val slow = SlowWork(looper.thread.name, AndroidLog)

    // The piece of work the thread is in now: when it began (0 for none) and its class.
    @Volatile private var runningSince = 0L
    @Volatile private var running: Any? = null

    // A thread of its own looks at the work every second: a piece that never ends is never
    // reported by the thread itself, and then nothing shows why the host's requests go unanswered.
    private val watchThread = HandlerThread("dsa-watch-" + looper.thread.name).apply { start() }
    private val watch = Handler(watchThread.looper)
    private val watcher = object : Runnable {
        override fun run() {
            val since = runningSince
            val work = running
            if (since != 0L && work != null) {
                slow.stuck(work, since, SystemClock.uptimeMillis()) {
                    looper.thread.stackTrace.map { "${it.className}.${it.methodName}" }
                }
            }
            watch.postDelayed(this, WATCH_MILLIS)
        }
    }

    init {
        watch.postDelayed(watcher, WATCH_MILLIS)
    }

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
        running = work
        runningSince = started
        try {
            work()
        } catch (e: Exception) {
            AndroidLog.warn("the agent's work failed", e)
        } finally {
            runningSince = 0L
            running = null
        }
        slow.ran(work, started, SystemClock.uptimeMillis())
    }

    private companion object {
        const val WATCH_MILLIS = 1_000L
    }
}
